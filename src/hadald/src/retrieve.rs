//! Retrieval over the RAG index.
//!
//! # Why this lives in hadald and not the broker
//!
//! The broker owns the prompt — that is the thesis, and it still does: it
//! decides *whether* to retrieve and *where* the passages go. What is
//! delegated here is ranking, for two reasons.
//!
//! The query has to be embedded by the same model that built the index, and
//! that model is reached through hadald. Splitting the embed call from the
//! search would mean the broker holding an embedding client it otherwise has
//! no use for.
//!
//! And the index holds **public documentation only** — man pages, package
//! READMEs, the Gentoo Handbook. No system state, nothing captured from this
//! machine. That is what makes the delegation acceptable, and it is a property
//! that has to be maintained: **if the index ever ingests journal excerpts,
//! build logs or config files, retrieval belongs back in the broker**, because
//! at that point ranking decides what leaves the machine.
//!
//! # Format
//!
//! `vectors.f32` is `n × dim` little-endian f32, already L2-normalised by
//! `build_index.py`. `chunks.jsonl` has one object per line, line `N`
//! describing vector `N`. That correspondence is the entire contract and is
//! checked on load — a mismatch means every passage is attributed to the wrong
//! source, which reads as plausible and is entirely wrong.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Chunk {
    /// `source/path:start-end`, shown to the model so it can cite.
    pub r#ref: String,
    pub text: String,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    model: String,
    dim: usize,
}

pub struct Index {
    dim: usize,
    /// Flat `n * dim`. One allocation; 7576×2048 f32 is 59 MiB, which is
    /// cheaper to hold than to mmap-and-fault per query.
    vectors: Vec<f32>,
    chunks: Vec<Chunk>,
    pub model: String,
}

/// Hand-written rather than derived: a derived `Debug` would print 59 MiB of
/// floats, and the only thing anyone wants from it is the shape.
impl std::fmt::Debug for Index {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Index")
            .field("model", &self.model)
            .field("dim", &self.dim)
            .field("chunks", &self.chunks.len())
            .finish()
    }
}

#[derive(Debug)]
pub enum IndexError {
    Missing(PathBuf),
    Malformed(String),
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::Missing(p) => write!(f, "index file missing: {}", p.display()),
            IndexError::Malformed(m) => write!(f, "index malformed: {m}"),
        }
    }
}

impl Index {
    pub fn load(dir: &Path) -> Result<Self, IndexError> {
        let manifest_path = dir.join("manifest.json");
        let vec_path = dir.join("vectors.f32");
        let jsonl_path = dir.join("chunks.jsonl");
        for p in [&manifest_path, &vec_path, &jsonl_path] {
            if !p.exists() {
                return Err(IndexError::Missing(p.clone()));
            }
        }

        let manifest: Manifest = serde_json::from_str(
            &std::fs::read_to_string(&manifest_path)
                .map_err(|e| IndexError::Malformed(e.to_string()))?,
        )
        .map_err(|e| IndexError::Malformed(format!("manifest: {e}")))?;

        let raw = std::fs::read(&vec_path).map_err(|e| IndexError::Malformed(e.to_string()))?;
        if raw.len() % (manifest.dim * 4) != 0 {
            return Err(IndexError::Malformed(format!(
                "vectors.f32 is {} bytes, not a multiple of dim {} × 4",
                raw.len(),
                manifest.dim
            )));
        }
        let vectors: Vec<f32> = raw
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();

        let text = std::fs::read_to_string(&jsonl_path)
            .map_err(|e| IndexError::Malformed(e.to_string()))?;
        let chunks: Vec<Chunk> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()
            .map_err(|e| IndexError::Malformed(format!("chunks.jsonl: {e}")))?;

        let n_vec = vectors.len() / manifest.dim;
        // The load-bearing check. Off-by-one here attributes every passage to
        // the wrong file, and the output still looks like a citation.
        if n_vec != chunks.len() {
            return Err(IndexError::Malformed(format!(
                "{n_vec} vectors but {} chunks — line N of chunks.jsonl must describe vector N; \
                 re-run rag/export_jsonl.py",
                chunks.len()
            )));
        }

        Ok(Index { dim: manifest.dim, vectors, chunks, model: manifest.model })
    }

    /// Chunk count. Also the vector count — the two are equal by construction
    /// and that is checked at load.
    pub fn len(&self) -> usize {
        self.chunks.len()
    }


    /// Top-`k` by cosine similarity.
    ///
    /// Stored vectors are already normalised, so this normalises only the
    /// query and takes dot products — a full scan of 7576×2048 is ~15M
    /// multiply-adds, well under a millisecond, and an exact answer beats an
    /// approximate index at this size.
    pub fn search(&self, query: &[f32], k: usize) -> Vec<(f32, &Chunk)> {
        if query.len() != self.dim || self.chunks.is_empty() {
            return Vec::new();
        }
        let norm = query.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm = if norm == 0.0 { 1.0 } else { norm };

        let mut scored: Vec<(f32, usize)> = (0..self.chunks.len())
            .map(|i| {
                let row = &self.vectors[i * self.dim..(i + 1) * self.dim];
                let dot: f32 = row.iter().zip(query).map(|(a, b)| a * b).sum();
                (dot / norm, i)
            })
            .collect();

        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        scored.truncate(k);
        scored.into_iter().map(|(s, i)| (s, &self.chunks[i])).collect()
    }
}

/// Render passages for the prompt.
///
/// Each carries its `ref` so the model can cite a real location rather than
/// inventing one — which is the entire point. A passage without provenance is
/// indistinguishable from a hallucination once it reaches the answer.
pub fn format_passages(hits: &[(f32, &Chunk)]) -> String {
    let mut out = String::new();
    for (score, chunk) in hits {
        out.push_str(&format!("[{}] (relevance {score:.2})\n{}\n\n", chunk.r#ref, chunk.text));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_index(dir: &Path, dim: usize, vecs: &[Vec<f32>], refs: &[&str]) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            format!(r#"{{"model":"test","dim":{dim}}}"#),
        )
        .unwrap();
        let mut f = std::fs::File::create(dir.join("vectors.f32")).unwrap();
        for v in vecs {
            for x in v {
                f.write_all(&x.to_le_bytes()).unwrap();
            }
        }
        let mut j = std::fs::File::create(dir.join("chunks.jsonl")).unwrap();
        for r in refs {
            writeln!(j, r#"{{"ref":"{r}","text":"body of {r}"}}"#).unwrap();
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("hadald-idx-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn loads_and_ranks_by_similarity() {
        let dir = tmp("ok");
        write_index(
            &dir,
            2,
            &[vec![1.0, 0.0], vec![0.0, 1.0], vec![0.7071, 0.7071]],
            &["a:1-1", "b:1-1", "c:1-1"],
        );
        let idx = Index::load(&dir).unwrap();
        assert_eq!(idx.len(), 3);

        let hits = idx.search(&[1.0, 0.0], 2);
        assert_eq!(hits[0].1.r#ref, "a:1-1", "exact match should rank first");
        assert_eq!(hits[1].1.r#ref, "c:1-1", "45° should beat orthogonal");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The failure that produces confident, wrongly-attributed citations.
    #[test]
    fn refuses_an_index_whose_counts_disagree() {
        let dir = tmp("mismatch");
        write_index(&dir, 2, &[vec![1.0, 0.0], vec![0.0, 1.0]], &["only-one:1-1"]);
        let err = Index::load(&dir).expect_err("2 vectors, 1 chunk must not load");
        assert!(format!("{err}").contains("must describe vector"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_a_truncated_vector_file() {
        let dir = tmp("truncated");
        write_index(&dir, 2, &[vec![1.0, 0.0]], &["a:1-1"]);
        // Lop off one float: no longer a whole number of rows.
        let p = dir.join("vectors.f32");
        let mut b = std::fs::read(&p).unwrap();
        b.truncate(b.len() - 4);
        std::fs::write(&p, b).unwrap();
        assert!(Index::load(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_wrong_dimension_query_returns_nothing_rather_than_garbage() {
        let dir = tmp("dim");
        write_index(&dir, 2, &[vec![1.0, 0.0]], &["a:1-1"]);
        let idx = Index::load(&dir).unwrap();
        assert!(idx.search(&[1.0, 0.0, 0.0], 1).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn passages_carry_their_reference() {
        let dir = tmp("fmt");
        write_index(&dir, 2, &[vec![1.0, 0.0]], &["man/kernel-install.8:120-160"]);
        let idx = Index::load(&dir).unwrap();
        let text = format_passages(&idx.search(&[1.0, 0.0], 1));
        assert!(text.contains("man/kernel-install.8:120-160"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

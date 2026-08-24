//! Synthetic sysfs trees, for testing hardware this machine does not have.
//!
//! docs/compute.md §6 step 1 says every finding so far came from one
//! integrated GPU, and that the AMD/CCX and discrete paths are "written and
//! unobserved". Waiting for hardware to test them means they stay unobserved
//! until the tower is set up, and §6a says the tower cannot answer all of it
//! anyway.
//!
//! A fixture is not a substitute for running on real hardware — it tests what
//! this code does with a given sysfs layout, not whether that layout is what
//! the driver actually writes. The layouts here are drawn from attribute names
//! and value shapes observed on this laptop or documented per-driver, and the
//! honest limit is stated in each test that relies on a shape nobody has seen.
//!
//! Test-only. No dependencies, so temp directories are built by hand and
//! removed on drop.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static SEQ: AtomicU32 = AtomicU32::new(0);

pub struct Fixture {
    root: PathBuf,
}

impl Fixture {
    pub fn new(name: &str) -> Self {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "sonar-fixture-{}-{}-{}",
            std::process::id(),
            n,
            name
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create fixture root");
        Fixture { root }
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    fn write(&self, rel: impl AsRef<Path>, contents: &str) {
        let p = self.root.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create fixture dir");
        }
        // Trailing newline, as sysfs writes. The parsers trim, and a fixture
        // that omitted it would pass while the real thing failed.
        fs::write(&p, format!("{contents}\n")).expect("write fixture file");
    }

    // ------------------------------------------------------------ GPU

    /// A DRM card. `vram` present means a dedicated-memory attribute is
    /// written, under the name the given driver actually uses.
    pub fn gpu_card(&self, n: u32, driver: &str, vendor: &str, device: &str, vram: Option<u64>) {
        let dev = format!("card{n}/device");
        self.write(format!("{dev}/vendor"), vendor);
        self.write(format!("{dev}/device"), device);
        self.write(format!("{dev}/numa_node"), "-1");

        if let Some(bytes) = vram {
            let attr = match driver {
                "i915" | "xe" => "lmem_total_bytes",
                _ => "mem_info_vram_total",
            };
            self.write(format!("{dev}/{attr}"), &bytes.to_string());
        }

        // sysfs presents `device/driver` as a symlink into the driver tree; the
        // code takes its file_name. A dangling relative link is enough and
        // avoids fabricating a whole PCI tree.
        let link = self.root.join(format!("{dev}/driver"));
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(format!("../../../bus/pci/drivers/{driver}"), &link)
            .expect("symlink driver");
    }

    pub fn connector(&self, card: &str, name: &str, status: &str) {
        self.write(format!("{card}-{name}/status"), status);
    }

    pub fn render_node(&self, n: u32) {
        self.write(format!("renderD{}/dev", 128 + n), "226:128");
    }

    // ------------------------------------------------------------ CPU

    pub fn present(&self, cpulist: &str) {
        self.write("present", cpulist);
    }

    /// One logical CPU. `l2_shared` is the cpulist sharing this L2, which is
    /// what distinguishes an E-core cluster or a CCX from a private cache.
    #[allow(clippy::too_many_arguments)]
    pub fn cpu(
        &self,
        id: u32,
        max_khz: u32,
        core_cpus_list: &str,
        core_id: u32,
        l2_kb: u32,
        l2_shared: &str,
        capacity: Option<u32>,
    ) {
        let d = format!("cpu{id}");
        self.write(format!("{d}/cpufreq/cpuinfo_max_freq"), &max_khz.to_string());
        self.write(format!("{d}/topology/core_cpus_list"), core_cpus_list);
        self.write(format!("{d}/topology/core_id"), &core_id.to_string());
        self.write(format!("{d}/cache/index2/level"), "2");
        self.write(format!("{d}/cache/index2/size"), &format!("{l2_kb}K"));
        self.write(format!("{d}/cache/index2/shared_cpu_list"), l2_shared);
        if let Some(c) = capacity {
            self.write(format!("{d}/cpu_capacity"), &c.to_string());
        }
    }

    /// An L3 entry, for the cache level that distinguishes AMD CCXs.
    pub fn cpu_l3(&self, id: u32, l3_kb: u32, shared: &str) {
        let d = format!("cpu{id}");
        self.write(format!("{d}/cache/index3/level"), "3");
        self.write(format!("{d}/cache/index3/size"), &format!("{l3_kb}K"));
        self.write(format!("{d}/cache/index3/shared_cpu_list"), shared);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

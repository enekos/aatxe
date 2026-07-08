//! Execution isolation for bench runs.
//!
//! A bench command runs in one of two places:
//!
//! * [`Isolation::Local`] — directly on the host. The default; today's
//!   behaviour, unchanged.
//! * [`Isolation::Microvm`] — inside an ephemeral libkrun microVM driven by
//!   the [`krunvm`](https://github.com/containers/krunvm) CLI. Pinned vCPUs,
//!   a fixed RAM budget, and a clean rootfs with no host noise give far more
//!   *predictable* bench numbers — which is the whole point of benching in a
//!   VM. Works on macOS (Hypervisor.framework) and Linux (KVM) with the same
//!   code path.
//!
//! We shell out to the `krunvm` binary rather than link `libkrun`, so aatxe
//! gains **no** new build-time dependency and no FFI. The tradeoff is that
//! `krunvm` must be installed (`make microvm-setup`); when it isn't, the
//! microVM path fails with an actionable install hint instead of a cryptic
//! spawn error.
//!
//! ## How a guest run is wired
//!
//! `krunvm create` bakes the VM's resources + volume mounts, then
//! `krunvm start` execs a command inside it. We:
//!
//! 1. mount the working directory at the *same* absolute path in the guest,
//!    so paths in the command line resolve identically;
//! 2. mount a persistent host cache dir at [`GUEST_CACHE_MOUNT`] and point
//!    `CARGO_TARGET_DIR` / `CARGO_HOME` / `GOCACHE` / … at it. This keeps
//!    guest builds incremental across runs *and* off the host's own
//!    `target/` (which is a different architecture — sharing it would
//!    corrupt both);
//! 3. exec the command through `/bin/sh` with a normalised `PATH` so the
//!    language toolchains in the stock `rust` / `golang` / `node` images are
//!    always found.
//!
//! The VM is deleted on drop ([`VmGuard`]) so runs leave nothing behind.

use crate::cli::{IsolationArg, VmOpts};
use aatxe_core::types::Language;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

/// Default guest vCPU count when `--vm-cpus` / `AATXE_VM_CPUS` is unset.
pub const DEFAULT_VM_CPUS: u32 = 2;
/// Default guest RAM (MiB) when `--vm-mem` / `AATXE_VM_MEM` is unset.
pub const DEFAULT_VM_MEM_MIB: u32 = 2048;

/// Where the persistent build-cache volume is mounted inside the guest.
pub const GUEST_CACHE_MOUNT: &str = "/aatxe-cache";

/// A `PATH` covering the toolchain locations of the stock language images
/// (`/usr/local/cargo/bin` for rust, `/usr/local/go/bin` for golang, …)
/// plus the standard dirs. Prepended to the image's own `$PATH` so a
/// custom `--vm-image` still works.
const GUEST_PATH: &str =
    "/usr/local/cargo/bin:/usr/local/go/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Where a bench command executes.
#[derive(Clone, Debug)]
pub enum Isolation {
    /// Directly on the host.
    Local,
    /// Inside an ephemeral libkrun microVM.
    Microvm(MicrovmConfig),
}

/// Resolved microVM parameters. Constructed from [`VmOpts`] via
/// [`Isolation::from_opts`].
#[derive(Clone, Debug)]
pub struct MicrovmConfig {
    /// OCI image for the guest rootfs. Must carry the language toolchain.
    pub image: String,
    /// Guest vCPUs.
    pub cpus: u32,
    /// Guest RAM in MiB.
    pub mem_mib: u32,
    /// Host directory mounted at [`GUEST_CACHE_MOUNT`] to persist build
    /// caches between runs.
    pub cache_dir: PathBuf,
}

/// The default OCI image for a language — a stock upstream image that ships
/// the toolchain (and a C compiler, needed by e.g. tree-sitter crates).
pub fn default_image(lang: Language) -> &'static str {
    match lang {
        Language::Rust => "docker.io/library/rust:1",
        Language::Go => "docker.io/library/golang:1",
        Language::Ts => "docker.io/library/node:22",
    }
}

impl Isolation {
    /// Resolve CLI/env options into an [`Isolation`]. `lang` selects the
    /// default guest image when `--vm-image` is not given.
    pub fn from_opts(opts: &VmOpts, lang: Language) -> Result<Self> {
        match opts.isolation {
            IsolationArg::Local => Ok(Isolation::Local),
            IsolationArg::Microvm => Ok(Isolation::Microvm(MicrovmConfig {
                image: opts
                    .vm_image
                    .clone()
                    .unwrap_or_else(|| default_image(lang).to_string()),
                cpus: opts.vm_cpus,
                mem_mib: opts.vm_mem,
                cache_dir: default_cache_dir()?,
            })),
        }
    }

    pub fn is_microvm(&self) -> bool {
        matches!(self, Isolation::Microvm(_))
    }

    /// Run `script` under `/bin/sh -c` in `cwd` with `env` set, capturing
    /// stdout. `stderr` streams to the caller's terminal (build logs), so
    /// the returned [`Output`] carries only stdout.
    ///
    /// * `Local` runs the script on the host shell — equivalent to spawning
    ///   the program directly, but lets both callers share one entry point.
    /// * `Microvm` runs it inside a fresh guest.
    pub fn run_script(&self, cwd: &Path, script: &str, env: &[(String, String)]) -> Result<Output> {
        match self {
            Isolation::Local => run_local(cwd, script, env),
            Isolation::Microvm(cfg) => run_microvm(cfg, cwd, script, env),
        }
    }
}

fn run_local(cwd: &Path, script: &str, env: &[(String, String)]) -> Result<Output> {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(script)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().context("spawning `/bin/sh -c` (local)")
}

fn run_microvm(
    cfg: &MicrovmConfig,
    cwd: &Path,
    script: &str,
    env: &[(String, String)],
) -> Result<Output> {
    let krunvm = ensure_krunvm()?;
    std::fs::create_dir_all(&cfg.cache_dir)
        .with_context(|| format!("creating microVM cache dir {}", cfg.cache_dir.display()))?;

    let name = unique_vm_name();
    eprintln!(
        "microvm: booting guest {name} · image {} · {} vCPU · {} MiB \
         (first run pulls the image — this can take a few minutes)",
        cfg.image, cfg.cpus, cfg.mem_mib
    );

    let create = create_argv(cfg, &name, cwd);
    let status = Command::new(&krunvm)
        .args(&create)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("spawning `krunvm create`")?;
    if !status.success() {
        bail!(
            "`krunvm create` failed ({status}) — is the image '{}' reachable and is \
             virtualization available? Try `make microvm-doctor`.",
            cfg.image
        );
    }
    // From here on the VM exists; delete it no matter how we leave.
    let _guard = VmGuard {
        name: name.clone(),
        krunvm: krunvm.clone(),
    };

    // Cache-redirect env wins nothing over the caller's explicit env
    // (no keys overlap), but is listed first so caller env can override.
    let mut full_env = cache_env();
    full_env.extend_from_slice(env);

    let guest = guest_argv(script, &full_env);
    let output = Command::new(&krunvm)
        .arg("start")
        .arg(&name)
        .args(&guest)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .context("spawning `krunvm start`")?;
    Ok(output)
}

/// Build the `krunvm create …` argument vector. Options first, image last.
fn create_argv(cfg: &MicrovmConfig, name: &str, cwd: &Path) -> Vec<String> {
    let cwd = cwd.display().to_string();
    vec![
        "create".to_string(),
        "--name".to_string(),
        name.to_string(),
        "--cpus".to_string(),
        cfg.cpus.to_string(),
        "--mem".to_string(),
        cfg.mem_mib.to_string(),
        "--workdir".to_string(),
        cwd.clone(),
        // Repo/worktree at the same path so command lines resolve 1:1.
        "--volume".to_string(),
        format!("{cwd}:{cwd}"),
        // Persistent build cache.
        "--volume".to_string(),
        format!("{}:{}", cfg.cache_dir.display(), GUEST_CACHE_MOUNT),
        cfg.image.clone(),
    ]
}

/// The tokens that follow `krunvm start <name>`: run the script through
/// `/usr/bin/env` (to set vars) and `/bin/sh -c` (to normalise `PATH`).
fn guest_argv(script: &str, env: &[(String, String)]) -> Vec<String> {
    let mut v = vec!["/usr/bin/env".to_string()];
    for (k, val) in env {
        v.push(format!("{k}={val}"));
    }
    v.push("/bin/sh".to_string());
    v.push("-c".to_string());
    v.push(format!("export PATH=\"{GUEST_PATH}:$PATH\"\n{script}"));
    v
}

/// Guest environment redirecting each toolchain's cache into the persistent
/// mount. Harmless when a given toolchain isn't used.
fn cache_env() -> Vec<(String, String)> {
    let c = GUEST_CACHE_MOUNT;
    vec![
        ("CARGO_HOME".to_string(), format!("{c}/cargo")),
        ("CARGO_TARGET_DIR".to_string(), format!("{c}/target")),
        ("GOCACHE".to_string(), format!("{c}/go-build")),
        ("GOMODCACHE".to_string(), format!("{c}/go-mod")),
        ("npm_config_cache".to_string(), format!("{c}/npm")),
    ]
}

/// POSIX single-quote a token for safe interpolation into a shell string.
pub fn sh_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Join a program + args into a single `exec`-able shell command, each token
/// individually quoted. Used to lower a `program args…` runner invocation
/// into the script that [`Isolation::run_script`] expects.
pub fn exec_line(program: &str, args: &[&str]) -> String {
    let mut line = String::from("exec ");
    line.push_str(&sh_quote(program));
    for a in args {
        line.push(' ');
        line.push_str(&sh_quote(a));
    }
    line
}

fn default_cache_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .context("HOME is not set — cannot locate the microVM cache dir")?;
    Ok(PathBuf::from(home).join(".cache/aatxe/vm"))
}

/// Probe for the `krunvm` binary, returning an actionable error when it's
/// missing so the microVM path never dies on a bare "No such file".
fn ensure_krunvm() -> Result<PathBuf> {
    match Command::new("krunvm")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(_) => Ok(PathBuf::from("krunvm")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
            "`krunvm` not found on PATH — the microVM isolation backend is not installed.\n\
             Install it once with:\n    make microvm-setup\n\
             (brew tap slp/krun && brew install krunvm), then re-run with \
             `--isolation microvm`. Check readiness with `make microvm-doctor`."
        ),
        Err(e) => Err(anyhow::Error::new(e).context("probing for `krunvm`")),
    }
}

static VM_SEQ: AtomicU64 = AtomicU64::new(0);

fn unique_vm_name() -> String {
    format!(
        "aatxe-{}-{}",
        std::process::id(),
        VM_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// Deletes its microVM when dropped, so a run leaves no VMs behind even on
/// error or panic.
struct VmGuard {
    name: String,
    krunvm: PathBuf,
}

impl Drop for VmGuard {
    fn drop(&mut self) {
        let _ = Command::new(&self.krunvm)
            .args(["delete", &self.name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::VmOpts;

    fn micro_opts() -> VmOpts {
        VmOpts {
            isolation: IsolationArg::Microvm,
            vm_cpus: DEFAULT_VM_CPUS,
            vm_mem: DEFAULT_VM_MEM_MIB,
            vm_image: None,
        }
    }

    #[test]
    fn from_opts_local_is_local() {
        let opts = VmOpts {
            isolation: IsolationArg::Local,
            vm_cpus: 4,
            vm_mem: 4096,
            vm_image: Some("ignored".into()),
        };
        assert!(!Isolation::from_opts(&opts, Language::Rust)
            .unwrap()
            .is_microvm());
    }

    #[test]
    fn from_opts_microvm_defaults_image_per_language() {
        let iso = Isolation::from_opts(&micro_opts(), Language::Go).unwrap();
        match iso {
            Isolation::Microvm(cfg) => {
                assert_eq!(cfg.image, "docker.io/library/golang:1");
                assert_eq!(cfg.cpus, DEFAULT_VM_CPUS);
                assert_eq!(cfg.mem_mib, DEFAULT_VM_MEM_MIB);
            }
            Isolation::Local => panic!("expected microvm"),
        }
    }

    #[test]
    fn from_opts_microvm_honours_explicit_image_and_resources() {
        let opts = VmOpts {
            isolation: IsolationArg::Microvm,
            vm_cpus: 8,
            vm_mem: 8192,
            vm_image: Some("my/custom:tag".into()),
        };
        match Isolation::from_opts(&opts, Language::Rust).unwrap() {
            Isolation::Microvm(cfg) => {
                assert_eq!(cfg.image, "my/custom:tag");
                assert_eq!(cfg.cpus, 8);
                assert_eq!(cfg.mem_mib, 8192);
            }
            Isolation::Local => panic!("expected microvm"),
        }
    }

    #[test]
    fn default_image_covers_every_language() {
        assert_eq!(default_image(Language::Rust), "docker.io/library/rust:1");
        assert_eq!(default_image(Language::Go), "docker.io/library/golang:1");
        assert_eq!(default_image(Language::Ts), "docker.io/library/node:22");
    }

    #[test]
    fn create_argv_puts_options_first_image_last() {
        let cfg = MicrovmConfig {
            image: "docker.io/library/rust:1".into(),
            cpus: 2,
            mem_mib: 2048,
            cache_dir: PathBuf::from("/home/u/.cache/aatxe/vm"),
        };
        let argv = create_argv(&cfg, "aatxe-1-0", Path::new("/repo/aatxe"));
        assert_eq!(argv[0], "create");
        assert_eq!(argv.last().unwrap(), "docker.io/library/rust:1");
        // Working dir mounted 1:1.
        assert!(argv.contains(&"/repo/aatxe:/repo/aatxe".to_string()));
        // Cache volume mounted at the well-known guest path.
        assert!(argv.contains(&format!("/home/u/.cache/aatxe/vm:{GUEST_CACHE_MOUNT}")));
        assert!(argv.contains(&"--cpus".to_string()));
        assert!(argv.contains(&"2".to_string()));
    }

    #[test]
    fn guest_argv_sets_env_then_shells_with_path_fix() {
        let env = vec![("AATXE_SERVICE".to_string(), "svc".to_string())];
        let argv = guest_argv("exec ./bench", &env);
        assert_eq!(argv[0], "/usr/bin/env");
        assert_eq!(argv[1], "AATXE_SERVICE=svc");
        assert_eq!(argv[2], "/bin/sh");
        assert_eq!(argv[3], "-c");
        assert!(argv[4].contains("export PATH="));
        assert!(argv[4].contains("/usr/local/cargo/bin"));
        assert!(argv[4].trim_end().ends_with("exec ./bench"));
    }

    #[test]
    fn cache_env_redirects_all_toolchains_into_the_mount() {
        let env = cache_env();
        for key in [
            "CARGO_HOME",
            "CARGO_TARGET_DIR",
            "GOCACHE",
            "GOMODCACHE",
            "npm_config_cache",
        ] {
            let (_, v) = env.iter().find(|(k, _)| k == key).expect("key present");
            assert!(v.starts_with(GUEST_CACHE_MOUNT), "{key} -> {v}");
        }
    }

    #[test]
    fn sh_quote_wraps_and_escapes_single_quotes() {
        assert_eq!(sh_quote("plain"), "'plain'");
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn exec_line_quotes_each_token() {
        assert_eq!(
            exec_line("cargo", &["run", "--release", "--bin", "x"]),
            "exec 'cargo' 'run' '--release' '--bin' 'x'"
        );
    }
}

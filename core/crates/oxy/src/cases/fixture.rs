//! The sandbox `tests/cases.py` builds, ported so `oxy test --cases` runs
//! the same fixture: two throwaway repos under a tempdir `$HOME/repos`
//! (`oxy-fixture` in the shape the git cases describe, `omarchy` for the
//! `repo:` cases, `gum` for the clean-repo cases), a `fakebin` of stubs on
//! PATH (`claude`, `alacritty`, `oxy-volume`), and the whole XDG layout
//! pointed inside so nothing the engine writes — frecency, recents, caches —
//! touches the real machine.
//!
//! The env cannot be changed for this process (`env::set_var` is unsafe
//! here), so the runner re-executes itself with the overlay set on the
//! child. `OXY_CASES_INNER` marks the inner run.

use std::path::Path;
use std::process::Command;

/// Set on the re-executed child so it runs the cases instead of building
/// another sandbox.
const INNER: &str = "OXY_CASES_INNER";

/// Build the sandbox, re-run this binary inside it, and return its exit
/// code. `args` are this process's own arguments, forwarded verbatim.
pub(crate) fn sandboxed(args: &[String]) -> i32 {
    let sandbox = std::env::temp_dir().join(format!("oxy-cases-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&sandbox);
    if let Err(e) = build(&sandbox) {
        eprintln!("fixture build failed: {e} — git-family cases will fail");
    }

    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("cannot re-exec for the sandbox: {e}");
            let _ = std::fs::remove_dir_all(&sandbox);
            return 1;
        }
    };

    let repos = sandbox.join("repos");
    let fakebin = sandbox.join("fakebin");
    let path = std::env::join_paths(std::iter::once(fakebin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap_or_default();

    let mut cmd = Command::new(exe);
    cmd.args(args)
        .env(INNER, "1")
        .env("HOME", &sandbox)
        .env("USERPROFILE", &sandbox)
        .env("XDG_CONFIG_HOME", &sandbox)
        .env("XDG_STATE_HOME", sandbox.join("state"))
        .env("XDG_CACHE_HOME", sandbox.join("cache"))
        .env("OXY_REPO_ROOTS", &repos)
        .env("OXY_REPO", repos.join("oxy-fixture"))
        .env("PATH", path);
    if cfg!(unix) {
        cmd.env("XDG_RUNTIME_DIR", sandbox.join("run"));
    }

    let code = cmd
        .status()
        .map(|s| s.code().unwrap_or(1))
        .unwrap_or_else(|e| {
            eprintln!("cannot spawn the sandboxed run: {e}");
            1
        });
    let _ = std::fs::remove_dir_all(&sandbox);
    code
}

/// True for the inner run — the one the env overlay was set on.
pub(crate) fn inside() -> bool {
    std::env::var_os(INNER).is_some()
}

fn build(sandbox: &Path) -> std::io::Result<()> {
    let repos = sandbox.join("repos");
    std::fs::create_dir_all(&repos)?;
    std::fs::create_dir_all(sandbox.join("run"))?;

    if git_ok() {
        build_repos(&repos)?;
    }
    stub_bin(sandbox)?;
    write_config(sandbox, &repos)
}

fn git_ok() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn git(repo: &Path, args: &[&str]) -> std::io::Result<()> {
    let ok = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if ok {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("git {args:?} failed")))
    }
}

fn write(repo: &Path, name: &str, body: &str) -> std::io::Result<()> {
    std::fs::write(repo.join(name), body)
}

/// `oxy-fixture` carries the shapes the cases name: a `login` branch two
/// commits ahead of and one behind `main` (trunkAhead 2, trunkBehind 1), an
/// `other` branch, two stashes newest-first ("trying the other approach"
/// over "half a refactor"), and a dirty tree so Enter's diff and the
/// switch-refusal warning have something to say.
fn build_repos(repos: &Path) -> std::io::Result<()> {
    let fx = repos.join("oxy-fixture");
    git(repos, &["init", "-q", "-b", "main", "oxy-fixture"])?;
    git(&fx, &["config", "user.email", "t@t"])?;
    git(&fx, &["config", "user.name", "t"])?;
    git(
        &fx,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example/oxy-fixture.git",
        ],
    )?;
    write(&fx, "app.py", "print('hi')\n")?;
    git(&fx, &["add", "app.py"])?;
    git(&fx, &["commit", "-qm", "init"])?;

    git(&fx, &["checkout", "-qb", "login"])?;
    for i in 1..=2 {
        write(&fx, &format!("feat{i}.py"), &format!("# {i}\n"))?;
        git(&fx, &["add", &format!("feat{i}.py")])?;
        git(&fx, &["commit", "-qm", &format!("login work {i}")])?;
    }
    git(&fx, &["checkout", "-q", "main"])?;
    write(&fx, "README.md", "readme\n")?;
    git(&fx, &["add", "README.md"])?;
    git(&fx, &["commit", "-qm", "document"])?;
    git(&fx, &["branch", "other"])?;

    write(&fx, "app.py", "print('v2')\n")?;
    write(&fx, "sketch.txt", "half drawn\n")?;
    git(&fx, &["stash", "push", "-qum", "half a refactor"])?;
    write(&fx, "app.py", "print('v3')\n")?;
    git(&fx, &["stash", "push", "-qm", "trying the other approach"])?;
    write(&fx, "app.py", "print('v4')\n")?;

    for (name, remote) in [
        ("omarchy", "https://github.com/example/omarchy.git"),
        ("gum", ""),
    ] {
        let repo = repos.join(name);
        git(repos, &["init", "-q", "-b", "main", name])?;
        git(&repo, &["config", "user.email", "t@t"])?;
        git(&repo, &["config", "user.name", "t"])?;
        if !remote.is_empty() {
            git(&repo, &["remote", "add", "origin", remote])?;
        }
        write(&repo, "f", "x\n")?;
        git(&repo, &["add", "f"])?;
        git(&repo, &["commit", "-qm", "init"])?;
    }
    Ok(())
}

/// The CLIs a `when` or a case probes for but a sandbox should never really
/// run: `claude` makes the draft build, `alacritty` lets `terminal`
/// resolve, `oxy-volume` is what the volume sentence routes to. The `.cmd`
/// twins exist because PATHEXT is what Windows looks up.
fn stub_bin(sandbox: &Path) -> std::io::Result<()> {
    let fakebin = sandbox.join("fakebin");
    std::fs::create_dir_all(&fakebin)?;
    for name in ["claude", "alacritty", "oxy-volume"] {
        for suffix in ["", ".cmd"] {
            let p = fakebin.join(format!("{name}{suffix}"));
            std::fs::write(&p, "#!/usr/bin/env bash\necho stub-agent\n")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))?;
            }
        }
    }

    // ------------------------------------------------------------ docker
    // A canned daemon for the docker cases: `info` answers a fixed
    // ServerVersion (the extension's `when` and the provider's per-query
    // gate), `ps` four ids, `inspect` one object per line in ps order —
    // a restarting container first, the two running ones apart, an exited
    // one between, so the band sort has something to prove — and `stats`
    // nothing, the cold-start answer. Touching `$HOME/oxy-docker-down`
    // makes every call fail, which is how the absent-daemon case fakes a
    // stopped daemon.
    let docker = r#"#!/usr/bin/env bash
[ -f "$HOME/oxy-docker-down" ] && exit 1
case "$1" in
  info) echo "25.0.0" ;;
  ps) printf '%s\n' cccc3333dddd aaaa1111bbbb dddd4444eeee bbbb2222cccc ;;
  stats) exit 0 ;;
  inspect)
    cat <<'JSON'
{"id":"cccc3333dddd4444eeee5555ffff6666aaaa77778888bbbb","name":"/crashy","image":"broken:latest","restarts":7,"state":{"Status":"restarting","ExitCode":1,"OOMKilled":false,"StartedAt":"2025-05-30T12:00:00Z","FinishedAt":"2025-06-01T00:00:00Z"},"ports":{},"memCap":0,"nanoCpus":0,"project":"","service":""}
{"id":"aaaa1111bbbb2222cccc3333dddd4444eeee5555ffff6666","name":"/web","image":"nginx:1.27","restarts":0,"state":{"Status":"running","Running":true,"ExitCode":0,"OOMKilled":false,"StartedAt":"2025-01-01T00:00:00Z","Health":{"Status":"healthy"}},"ports":{"8080/tcp":[{"HostIp":"0.0.0.0","HostPort":"8080"},{"HostIp":"::","HostPort":"8080"}],"8443/tcp":[{"HostIp":"127.0.0.1","HostPort":"8443"}],"9090/tcp":[{"HostIp":"::1","HostPort":"9090"}],"6379/tcp":null},"memCap":536870912,"nanoCpus":150000000,"project":"shop","service":"storefront"}
{"id":"dddd4444eeee5555ffff6666aaaa77778888bbbb9999cccc","name":"/old","image":"alpine:3.20","restarts":0,"state":{"Status":"exited","ExitCode":0,"OOMKilled":false,"StartedAt":"2025-05-01T00:00:00Z","FinishedAt":"2025-06-01T12:00:00Z"},"ports":{"6379/tcp":null},"memCap":0,"nanoCpus":0,"project":"","service":""}
{"id":"bbbb2222cccc3333dddd4444eeee5555ffff6666aaaa7777","name":"/db","image":"postgres:17","restarts":0,"state":{"Status":"running","Running":true,"ExitCode":0,"OOMKilled":false,"StartedAt":"2025-01-01T00:00:00Z"},"ports":{"5432/tcp":[{"HostIp":"0.0.0.0","HostPort":"5432"}]},"memCap":0,"nanoCpus":0,"project":"shop","service":"postgres"}
JSON
    ;;
  *) exit 0 ;;
esac
"#;
    for suffix in ["", ".cmd"] {
        let p = fakebin.join(format!("docker{suffix}"));
        std::fs::write(&p, docker)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(())
}

/// The sandbox's config dir: the outer registry copied in (so the inner
/// run tests the same extensions the outer would have), and an `oxy.json`
/// carrying the fixture's repo roots through the settings channel — the
/// same value `OXY_REPO_ROOTS` hands the scripts, so both legs agree.
fn write_config(sandbox: &Path, repos: &Path) -> std::io::Result<()> {
    let oxy_dir = sandbox.join("omarchy/oxy");
    std::fs::create_dir_all(&oxy_dir)?;

    let src_exts = oxy_core::settings::paths::extensions_dir();
    let dst_exts = oxy_dir.join("extensions");
    if src_exts.is_dir() && src_exts != dst_exts {
        copy_dir(&src_exts, &dst_exts)?;
    }

    let roots = repos.to_string_lossy().replace('\\', "/");
    std::fs::write(
        sandbox.join("omarchy/oxy.json"),
        format!("{{\"extensionSettings\":{{\"repo\":{{\"roots\":\"{roots}\"}}}}}}\n"),
    )
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

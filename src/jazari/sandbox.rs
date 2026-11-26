//! One Docker container per product. It sees only the product's repo, mounted at the same path as
//! on the host so git worktrees resolve on both sides. It runs as a normal user with no Linux
//! capabilities and has fixed CPU, memory and process limits. Workers are `docker exec`'d into
//! it, one task at a time.

use std::io::Write;
use std::process::{Command, Stdio};

pub const IMAGE: &str = "jazari-worker:2";
const DOCKERFILE: &str = include_str!("../../assets/jazari/Dockerfile");

pub fn container(product: &str) -> String {
    format!("jazari-{product}")
}

/// The `docker run` arguments that create a product's container.
pub fn run_args(product: &str, repo: &str) -> Vec<String> {
    let mut a: Vec<String> = ["run", "--detach", "--name"].map(String::from).to_vec();
    a.push(container(product));
    a.extend(
        [
            "--memory", "2g", "--cpus", "2", "--pids-limit", "512", "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
            "--label", "marsad.jazari=1", "--volume",
        ]
        .map(String::from),
    );
    a.push(format!("{repo}:{repo}"));
    a.push(IMAGE.into());
    a
}

fn docker(args: &[&str]) -> anyhow::Result<std::process::Output> {
    Command::new("docker").args(args).stdin(Stdio::null()).output().map_err(|e| anyhow::anyhow!("docker couldn't run ({e}); is Docker Desktop running?"))
}

fn ensure_image() -> anyhow::Result<()> {
    if docker(&["image", "inspect", IMAGE])?.status.success() {
        return Ok(());
    }
    println!("Building the worker image {IMAGE} (a few minutes, once)…");
    let mut child = Command::new("docker").args(["build", "--quiet", "--tag", IMAGE, "-"]).stdin(Stdio::piped()).stdout(Stdio::null()).spawn()?;
    child.stdin.take().expect("stdin is piped").write_all(DOCKERFILE.as_bytes())?;
    anyhow::ensure!(child.wait()?.success(), "building {IMAGE} failed");
    Ok(())
}

/// Starts the product's container if it isn't running, creating it the first time. Returns its name.
pub fn ensure(product: &str, repo: &str) -> anyhow::Result<String> {
    let name = container(product);
    let state = docker(&["inspect", "--format", "{{.State.Running}} {{.Config.Image}} {{range .Mounts}}{{.Destination}}{{end}}", &name])?;
    if state.status.success() {
        let text = String::from_utf8_lossy(&state.stdout).trim().to_string();
        let mut parts = text.splitn(3, ' ');
        let (running, image, mount) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        if mount == repo && image == IMAGE {
            if running != "true" {
                anyhow::ensure!(docker(&["start", &name])?.status.success(), "couldn't start the container {name}");
            }
            return Ok(name);
        }
        // Made by an older jazari (another image or mount): the repo is on the host, so nothing is lost.
        anyhow::ensure!(docker(&["rm", "--force", &name])?.status.success(), "couldn't replace the container {name}");
    }
    anyhow::ensure!(std::path::Path::new(repo).is_dir(), "the repo folder {repo} doesn't exist");
    ensure_image()?;
    let args = run_args(product, repo);
    let out = Command::new("docker").args(&args).stdin(Stdio::null()).output()?;
    anyhow::ensure!(out.status.success(), "couldn't create the container {name}: {}", String::from_utf8_lossy(&out.stderr).trim());
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_container_sees_only_the_repo_and_has_limits() {
        let a = run_args("shop", "/Users/me/shop");
        let has = |k: &str, v: &str| a.windows(2).any(|w| w[0] == k && w[1] == v);
        assert!(has("--name", "jazari-shop") && has("--volume", "/Users/me/shop:/Users/me/shop") && has("--cap-drop", "ALL"));
        assert!(has("--memory", "2g") && has("--pids-limit", "512") && has("--security-opt", "no-new-privileges"));
        assert_eq!(a.iter().filter(|x| *x == "--volume").count(), 1);
        assert_eq!(a.last().unwrap(), IMAGE);
        assert!(DOCKERFILE.contains("USER node"));
    }
}

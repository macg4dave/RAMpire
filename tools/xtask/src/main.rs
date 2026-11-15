mod build_identity;
mod licenses;
mod local;
mod package;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const HELP: &str = r#"Repository build tooling

The gate and the packages: tools/build.sh runs these in the build
container.
  cargo xtask gate <format|build|test|lint|checks|local>
  cargo xtask licenses DIRECTORY   the third-party notices each package installs
  cargo xtask fixtures             this release's protocol fixtures, in
                                   fixtures/protocol/MAJOR.MINOR (a new minor
                                   release writes them; tools/release.sh)
"#;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match execute(&arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn execute(arguments: &[String]) -> Result<(), String> {
    let repo = repo_root()?;
    match arguments.first().map(String::as_str) {
        Some("help" | "--help" | "-h") | None => {
            print!("{HELP}\n{}", package::HELP);
            Ok(())
        }
        Some("gate") => gate(&arguments[1..], &repo),
        Some("package") => package::run(&package::parse(&arguments[1..])?, &repo),
        Some("fixtures") if arguments.len() == 1 => fixtures(&repo),
        Some("licenses") => match &arguments[1..] {
            [directory] => licenses::write(&repo, Path::new(directory)),
            _ => Err("licenses requires DIRECTORY".to_owned()),
        },
        Some(command) => Err(format!("unknown command: {command}\n\n{HELP}")),
    }
}

fn gate(arguments: &[String], repo: &Path) -> Result<(), String> {
    if arguments.len() != 1 {
        return Err("gate requires format|build|test|lint|checks|local".to_owned());
    }
    let step = match arguments[0].as_str() {
        "format" => local::Step::Format,
        "build" => local::Step::Build,
        "test" => local::Step::Test,
        "lint" => local::Step::Lint,
        "checks" => local::Step::Checks,
        "local" => local::Step::Local,
        value => return Err(format!("unknown gate: {value}")),
    };
    local::run(step, repo)
}

/// The documents this release's guest and controller exchange, kept so
/// the next minor release checks it still reads them
/// (crates/rampire-core/tests/version_skew.rs).
fn fixtures(repo: &Path) -> Result<(), String> {
    println!("==> protocol fixtures for this release");
    rampire_ops::process::run(
        "cargo",
        &[
            "test",
            "-p",
            "rampire-core",
            "--test",
            "version_skew",
            "--locked",
            "--",
            "--ignored",
            "--exact",
            "write_this_releases_fixtures",
        ],
        repo,
    )
}

fn repo_root() -> Result<PathBuf, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "xtask manifest is not nested under the repository root".to_owned())?;
    Ok(root.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_root_contains_workspace_manifest() {
        assert!(repo_root()
            .expect("repository root")
            .join("Cargo.toml")
            .is_file());
    }
}

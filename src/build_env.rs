use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::{tempdir, TempDir};

#[derive(Debug)]
pub struct BuildEnv {
    pub build_dir: PathBuf,
    pub source_dir: PathBuf,
    pub config: PathBuf,
    _temp_dir: Option<TempDir>,
}

fn check_cmd(cmd: &mut Command, message: &str) -> Output {
    let output = cmd.output().expect(message);
    if !output.status.success() {
        panic!("{message}: {output:#?}");
    }
    output
}

fn new_make_cmd() -> Command {
    let mut cmd = Command::new("make");
    cmd.arg(format!("-j{}", std::thread::available_parallelism().unwrap()));
    cmd
}

impl BuildEnv {
    pub fn from_dir(build_dir: &Path) -> Self {
        BuildEnv {
            build_dir: build_dir.to_path_buf(),
            source_dir: build_dir.join("source"),
            config: build_dir.join("config"),
            _temp_dir: None,
        }
    }

    pub fn from_config(source_dir: &Path, config: &Path, makeconf: Option<&Path>) -> Self {
        let temp_dir = tempdir().expect("Unable to create temporary build dir.");
        // The B= options requires a non-existing directory.
        let build_dir = temp_dir.path().join("build");
        // Initialize build directory.
        let mut build_dir_arg = OsString::from("B=");
        build_dir_arg.push(build_dir.as_os_str());
        check_cmd(
            new_make_cmd().arg(&build_dir_arg).current_dir(source_dir),
            "Unable to initialize temporary build dir.",
        );

        // Configure build directory.
        fs::copy(config, build_dir.join("globalconfig.out")).expect("Unable to copy config.");

        if let Some(makeconf_local) = makeconf {
            fs::copy(makeconf_local, build_dir.join("Makeconf.local"))
                .expect("Unable to copy makeconf.");
        }

        check_cmd(
            new_make_cmd().arg("olddefconfig").current_dir(&build_dir),
            "Unable to apply config.",
        );

        BuildEnv {
            build_dir,
            source_dir: source_dir.to_path_buf(),
            config: config.to_path_buf(),
            _temp_dir: Some(temp_dir),
        }
    }

    pub fn gen_compile_commands(&self) {
        // Make .Module.deps and compile_commands.json
        check_cmd(
            new_make_cmd()
                .args([".Modules.deps", "compile_commands.json"])
                .current_dir(&self.build_dir),
            "Unable to build.",
        );
    }
}

//! Compile Blueprint templates (`src/ui/*.blp`) to GtkBuilder XML in OUT_DIR.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let ui_dir = manifest.join("src/ui");

    let templates = ["window", "log_window"];
    for name in templates {
        println!("cargo:rerun-if-changed=src/ui/{name}.blp");
    }

    let compiler = find_compiler();
    println!("cargo:rerun-if-changed={}", compiler.display());

    for name in templates {
        let src = ui_dir.join(format!("{name}.blp"));
        let dest = out.join(format!("{name}.ui"));
        let status = Command::new(&compiler)
            .args(["compile", "--output"])
            .arg(&dest)
            .arg(&src)
            .status()
            .unwrap_or_else(|e| {
                panic!(
                    "failed to run blueprint-compiler ({}): {e}\n\
                     Install it with:  pip install --user blueprint-compiler\n\
                     or:               sudo dnf install blueprint-compiler",
                    compiler.display()
                )
            });
        if !status.success() {
            panic!("blueprint-compiler failed for {}", src.display());
        }
    }
}

fn find_compiler() -> PathBuf {
    if let Ok(explicit) = env::var("BLUEPRINT_COMPILER") {
        return PathBuf::from(explicit);
    }

    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            let candidate = dir.join("blueprint-compiler");
            if candidate.is_file() {
                return candidate;
            }
        }
    }

    if let Ok(home) = env::var("HOME") {
        let user = Path::new(&home).join(".local/bin/blueprint-compiler");
        if user.is_file() {
            return user;
        }
    }

    panic!(
        "blueprint-compiler not found on PATH or in ~/.local/bin.\n\
         Install it with:  pip install --user blueprint-compiler\n\
         or:               sudo dnf install blueprint-compiler"
    );
}

//! Genera la tabla de skills embebidas: recorre `<repo>/skills/<nombre>/**` y emite
//! un `include_str!` por fichero. Sin dependencias: solo `std`.

use std::fs;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .expect("leer skills/")
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            walk(&path, base, out);
        } else if path.is_file() {
            let rel = path
                .strip_prefix(base)
                .expect("ruta relativa")
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, path));
        }
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let skills = manifest.join("../../skills");
    println!("cargo:rerun-if-changed={}", skills.display());
    println!("cargo:rerun-if-changed=build.rs");

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(top) = fs::read_dir(&skills) {
        let mut dirs: Vec<PathBuf> = top.flatten().map(|e| e.path()).collect();
        dirs.sort();
        for d in dirs.into_iter().filter(|p| p.is_dir()) {
            // Solo carpetas con SKILL.md; README.md y similares de la raíz no se embeben.
            if d.join("SKILL.md").is_file() {
                walk(&d, &skills, &mut files);
            }
        }
    }

    let mut code = String::from("pub static EMBEDDED: &[(&str, &str)] = &[\n");
    for (rel, abs) in &files {
        let abs = abs.canonicalize().expect("canonicalize");
        println!("cargo:rerun-if-changed={}", abs.display());
        code.push_str(&format!(
            "    ({rel:?}, include_str!({:?})),\n",
            abs.to_string_lossy()
        ));
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("out dir")).join("skills_embedded.rs");
    fs::write(out, code).expect("escribir skills_embedded.rs");
}

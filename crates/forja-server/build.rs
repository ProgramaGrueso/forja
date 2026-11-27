use std::env;
use std::fs;
use std::path::Path;

const TEMPLATE: &str = "src/panel/index.html";

const CSS_FILES: &[&str] = &[
    "base.css",
    "layout.css",
    "resumen.css",
    "ejecuciones.css",
    "agentes.css",
    "telemetria.css",
    "cuotas.css",
    "ajustes.css",
    "orquestador.css",
    "terminales.css",
    "archivos.css",
];

const JS_FILES: &[&str] = &[
    "core.js",
    "resumen.js",
    "ejecuciones.js",
    "agentes.js",
    "telemetria.js",
    "cuotas.js",
    "ajustes.js",
    "orquestador.js",
    "terminales.js",
    "archivos.js",
];

fn read_part(path: &str) -> String {
    println!("cargo:rerun-if-changed={path}");
    fs::read_to_string(path).unwrap_or_else(|e| panic!("no se pudo leer {path}: {e}"))
}

fn concat_parts(dir: &str, files: &[&str]) -> String {
    let mut out = String::new();
    for (idx, file) in files.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        let rel = format!("{dir}/{file}");
        let body = read_part(&rel);
        out.push_str(&body);
        if !body.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

fn replace_once(template: &str, marker: &str, content: &str) -> String {
    let count = template.matches(marker).count();
    assert!(
        count == 1,
        "el marcador {marker} debe aparecer exactamente una vez en {TEMPLATE} (encontrado {count})"
    );
    template.replacen(marker, content.trim_end_matches('\n'), 1)
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let template = read_part(TEMPLATE);
    let css = concat_parts("src/panel/css", CSS_FILES);
    let js = concat_parts("src/panel/js", JS_FILES);

    let html = replace_once(&template, "{{CSS}}", &css);
    let html = replace_once(&html, "{{JS}}", &js);

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR no definido por Cargo");
    let dest = Path::new(&out_dir).join("panel.html");
    fs::write(&dest, html)
        .unwrap_or_else(|e| panic!("no se pudo escribir {}: {e}", dest.display()));
}

//! sil2argent command line.
//!
//!   sil2argent build <skeleton artifact.json> <out artifact.json> [--root DIR] <Actor>=<silverc artifact.json>...
//!   sil2argent verify <artifact.json> [--id <expected artifact id>] <Actor>=<silverc artifact.json>...
//!   sil2argent handles <artifact.json> <Actor>=<silverc artifact.json>...
//!   sil2argent links <app artifact.json> <published artifact.json>...
//!   sil2argent relativize <DIR> <file.json>...
//!
//! `build` transplants hand-written contracts into an argentc skeleton (see the library docs).
//! `verify` re-derives everything a published artifact claims about them, without argentc; with `--id` it also
//! requires the artifact id you pinned (a forged artifact with the same ABI re-derives consistently from forged
//! silverc artifacts, so the pin is what ties it to the reviewed contracts).
//! `handles` only checks the actor-type handles (for apps that argentc compiled itself).
//! `links` checks that an app links exactly the published artifacts (name and artifact id).
//! `relativize` strips the checkout directory from the absolute paths argentc records, in place.

use argent_artifact::Artifact;
use sil2argent::{check_links, relativize_paths, transplant, verify, verify_handles, ActorSource};

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("sil2argent: {msg}");
    std::process::exit(1);
}

fn read(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| fail(format!("read {path}: {e}")))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &str) -> T {
    serde_json::from_str(&read(path)).unwrap_or_else(|e| fail(format!("parse {path}: {e}")))
}

fn sources(specs: &[String]) -> Vec<ActorSource> {
    specs
        .iter()
        .map(|spec| {
            let (actor, path) = spec.split_once('=').unwrap_or_else(|| fail(format!("bad actor spec `{spec}`")));
            ActorSource { actor: actor.to_string(), label: path.to_string(), abi: read_json(path) }
        })
        .collect()
}

fn write_lf(path: &str, text: &str) {
    if let Some(dir) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(dir).unwrap_or_else(|e| fail(format!("mkdir {}: {e}", dir.display())));
    }
    std::fs::write(path, text).unwrap_or_else(|e| fail(format!("write {path}: {e}")));
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("build") if args.len() >= 4 => {
            let mut rest: Vec<String> = args[3..].to_vec();
            let mut root = None;
            if let Some(i) = rest.iter().position(|a| a == "--root") {
                if i + 1 >= rest.len() {
                    fail("--root needs a directory");
                }
                root = Some(rest.remove(i + 1));
                rest.remove(i);
            }
            let mut art: Artifact = read_json(&args[1]);
            let log = transplant(&mut art, &sources(&rest)).unwrap_or_else(|e| fail(e));
            for line in &log {
                eprintln!("{line}");
            }
            let mut json = serde_json::to_string_pretty(&art).unwrap_or_else(|e| fail(format!("serialize: {e}"))) + "\n";
            if let Some(root) = root {
                json = relativize_paths(&json, std::path::Path::new(&root));
            }
            write_lf(&args[2], &json);
            eprintln!("wrote {} (app {}, id {})", args[2], art.app, art.id);
        }
        Some("verify") if args.len() >= 3 => {
            let art: Artifact = read_json(&args[1]);
            let mut rest: Vec<String> = args[2..].to_vec();
            if let Some(i) = rest.iter().position(|a| a == "--id") {
                if i + 1 >= rest.len() {
                    fail("--id needs the expected artifact id");
                }
                let want = rest.remove(i + 1);
                rest.remove(i);
                if art.id != want {
                    fail(format!("{}: artifact id {} is not the pinned id {want}", args[1], art.id));
                }
            }
            for line in verify(&art, &sources(&rest)).unwrap_or_else(|e| fail(e)) {
                eprintln!("{line}");
            }
            eprintln!("ok {} (app {}, id {})", args[1], art.app, art.id);
        }
        Some("handles") if args.len() >= 3 => {
            let art: Artifact = read_json(&args[1]);
            for line in verify_handles(&art, &sources(&args[2..])).unwrap_or_else(|e| fail(e)) {
                eprintln!("{line}");
            }
            eprintln!("ok {} (app {}, id {})", args[1], art.app, art.id);
        }
        Some("links") if args.len() >= 3 => {
            let art: Artifact = read_json(&args[1]);
            let published: Vec<Artifact> = args[2..].iter().map(|p| read_json(p)).collect();
            for line in check_links(&art, &published).unwrap_or_else(|e| fail(e)) {
                eprintln!("{line}");
            }
        }
        Some("relativize") if args.len() >= 3 => {
            for file in &args[2..] {
                let text = read(file);
                write_lf(file, &relativize_paths(&text, std::path::Path::new(&args[1])));
            }
        }
        _ => fail(
            "usage:\n  sil2argent build <skeleton.json> <out.json> [--root DIR] <Actor>=<silverc.json>...\n  \
             sil2argent verify <artifact.json> [--id <artifact id>] <Actor>=<silverc.json>...\n  \
             sil2argent handles <artifact.json> <Actor>=<silverc.json>...\n  \
             sil2argent links <app artifact.json> <published artifact.json>...
  \n             sil2argent relativize <DIR> <file.json>...",
        ),
    }
}

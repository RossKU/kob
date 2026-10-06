//! sil2argent: publish hand-written SilverScript contracts as an Argent app artifact.
//!
//! The input is a *skeleton*: argentc's artifact for an interface app whose actors declare the
//! same state layout (field names, order and Sil types) and the same entries (names, argument
//! types) as the hand-written contracts, with no compiler-owned route context, hidden entry
//! parameters or witness recipes. For every listed actor [`transplant`]
//!
//! - replaces the embedded Sil contract (`sil_abi.contracts[Actor]`) by the silverc output of the
//!   hand-written source,
//! - recomputes the template receipt (`sil_template_hash`) and the external `actor_type_handle`
//!   template (prefix = bytecode before the state span, suffix = after),
//! - recomputes the exported interface fingerprint,
//!
//! then records provenance, recomputes the artifact id and runs `Artifact::check_consistency`.
//! It refuses anything the transplant cannot keep truthful. [`verify`] re-derives all of that
//! from a published artifact and the silverc artifacts, without argentc.

use std::collections::BTreeMap;
use std::path::Path;

use argent_artifact::{actor_interface_fingerprint_hex, Artifact, GeneratorArtifact, RuntimeFieldArtifact, TypeArtifact};
use silverscript_abi::{SilAbiArtifact, SilContractArtifact};

pub const GENERATOR_NAME: &str = "argentc+kob-sil2argent";
pub const GENERATOR_VERSION: &str = "0.1.0";

/// One hand-written actor: the Argent actor name (= the SilverScript contract name) and its
/// silverc artifact.
pub struct ActorSource {
    pub actor: String,
    /// Where the silverc artifact came from (informational, recorded in `modules`).
    pub label: String,
    pub abi: SilAbiArtifact,
}

pub type Result<T> = std::result::Result<T, String>;

fn types_of(fields: &[RuntimeFieldArtifact]) -> Vec<(String, TypeArtifact)> {
    fields.iter().map(|f| (f.name.clone(), f.ty.clone())).collect()
}

fn entry_signatures(c: &SilContractArtifact) -> BTreeMap<String, Vec<TypeArtifact>> {
    c.entries.iter().map(|(n, e)| (n.clone(), e.params.iter().map(|p| p.ty.clone()).collect())).collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The hand-written contract of `src`, after checking that the silverc artifact is sound.
fn hand_contract(src: &ActorSource) -> Result<&SilContractArtifact> {
    src.abi.check_consistency().map_err(|e| format!("{}: invalid silverc artifact: {e}", src.label))?;
    src.abi.contracts.get(&src.actor).ok_or_else(|| format!("{} has no contract `{}`", src.label, src.actor))
}

/// Bytes before and after the state span of a compiled contract: the actor-type handle template.
fn template_parts(c: &SilContractArtifact) -> Result<(Vec<u8>, Vec<u8>, [u8; 32])> {
    let code = &c.compiled.bytecode;
    let span = c.compiled.state_span;
    let end = span.offset.checked_add(span.len).filter(|e| *e <= code.len()).ok_or("state span outside the bytecode")?;
    Ok((code[..span.offset].to_vec(), code[end..].to_vec(), c.compiled.template_hash))
}

/// Replace the generated contracts of `art` (an argentc skeleton) by the hand-written ones.
/// Returns one log line per actor.
pub fn transplant(art: &mut Artifact, sources: &[ActorSource]) -> Result<Vec<String>> {
    art.check_consistency().map_err(|e| format!("skeleton is not consistent: {e}"))?;
    if !art.argent.template_plan.witness_recipes.is_empty() || !art.argent.template_plan.route_families.is_empty() {
        return Err("skeleton has route witnesses/families; hand-written actors cannot provide them".into());
    }
    let mut log = Vec::new();
    let mut modules = Vec::new();
    for src in sources {
        let actor = src.actor.as_str();
        let contract = hand_contract(src)?.clone();
        let skel = art.sil_abi.contracts.get(actor).ok_or_else(|| format!("skeleton has no contract `{actor}`"))?.clone();

        // Runtime state: same names, order and Sil types (this is the actor_type<State> layout).
        if types_of(&contract.runtime_state.fields) != types_of(&skel.runtime_state.fields) {
            return Err(format!("{actor}: runtime state of the hand-written contract differs from the skeleton state"));
        }
        if contract.compiled.state_span.len == 0 && !contract.runtime_state.fields.is_empty() {
            return Err(format!("{actor}: empty state span (constructor values are inlined; move them into state)"));
        }
        // Entries: same names and argument types (argument names may differ).
        if entry_signatures(&contract) != entry_signatures(&skel) {
            return Err(format!(
                "{actor}: entry ABI differs: hand {:?} vs skeleton {:?}",
                entry_signatures(&contract),
                entry_signatures(&skel)
            ));
        }
        let actor_art = art.argent.actors.iter().find(|a| a.name == actor).ok_or_else(|| format!("no actor `{actor}`"))?;
        for e in &actor_art.entries {
            if !e.hidden_params.is_empty() || !e.witnesses.is_empty() || !e.observes.is_empty() || !e.spawns.is_empty() {
                return Err(format!("{actor}.{}: skeleton entry has compiler witnesses/observes/spawns; not representable", e.name));
            }
        }

        // Transplant the contract and the structs it uses.
        for (name, st) in &src.abi.structs {
            match art.sil_abi.structs.get(name) {
                Some(prev) if prev != st => return Err(format!("struct `{name}` differs between the skeleton and {}", src.label)),
                Some(_) => {}
                None => {
                    art.sil_abi.structs.insert(name.clone(), st.clone());
                }
            }
        }
        let (prefix, suffix, hash) = template_parts(&contract)?;
        let span = contract.compiled.state_span;
        let code_len = contract.compiled.bytecode.len();
        art.sil_abi.contracts.insert(actor.to_string(), contract.clone());

        let receipt = art
            .argent
            .template_plan
            .templates
            .iter_mut()
            .find(|t| t.actor == actor)
            .ok_or_else(|| format!("no template receipt for `{actor}`"))?;
        if !receipt.actor_type_handle.context_fields.is_empty() {
            return Err(format!("{actor}: skeleton handle has compiler-owned context fields"));
        }
        receipt.sil_template_hash = hash;
        receipt.actor_type_handle.template.prefix = prefix;
        receipt.actor_type_handle.template.suffix = suffix;
        receipt.actor_type_handle.template.hash = hash;

        let state = actor_art.state.clone();
        let fp = actor_interface_fingerprint_hex(actor, &state, &contract.runtime_state.fields)
            .map_err(|e| format!("{actor}: fingerprint: {e}"))?;
        let export = art
            .argent
            .interfaces
            .exports
            .iter_mut()
            .find(|i| i.actor == actor)
            .ok_or_else(|| format!("no exported interface for `{actor}`"))?;
        export.fingerprint_hex = fp;
        modules.push(format!("{} ({actor}, template {})", src.label, hex(&hash)));
        log.push(format!("{actor}: {code_len} B hand-written, state span {}+{}, handle {}", span.offset, span.len, hex(&hash)));
    }

    // Actors the transplant did not replace would still carry generated code: refuse.
    for actor in &art.argent.actors {
        if !sources.iter().any(|s| s.actor == actor.name) {
            return Err(format!("actor `{}` has no hand-written contract", actor.name));
        }
    }
    art.generator = GeneratorArtifact { name: GENERATOR_NAME.into(), version: GENERATOR_VERSION.into() };
    art.root = String::new();
    art.modules = modules;
    art.id = String::new();
    art.id = art.computed_id_hex().map_err(|e| format!("artifact id: {e}"))?;
    art.check_consistency().map_err(|e| format!("result is not consistent: {e}"))?;
    Ok(log)
}

/// Check that the artifact's template receipts and actor-type handles are the template of the
/// silverc artifacts (hash, prefix and suffix around the state span), and that the actor state
/// layout is the runtime state of the contract. Unlike [`verify`] it does not require the embedded
/// contract to be the silverc output, so it also covers an app compiled by argentc itself (the
/// KOB token) against the silverc build of the same program.
pub fn verify_handles(art: &Artifact, sources: &[ActorSource]) -> Result<Vec<String>> {
    art.check_consistency().map_err(|e| format!("artifact is not consistent: {e}"))?;
    art.verify_id().map_err(|e| format!("artifact id: {e}"))?;
    let mut log = Vec::new();
    for actor in &art.argent.actors {
        if !sources.iter().any(|s| s.actor == actor.name) {
            return Err(format!("actor `{}` is not backed by a silverc contract", actor.name));
        }
    }
    for src in sources {
        let actor = src.actor.as_str();
        let hand = hand_contract(src)?;
        let (prefix, suffix, hash) = template_parts(hand)?;
        let receipt = art
            .argent
            .template_plan
            .templates
            .iter()
            .find(|t| t.actor == actor)
            .ok_or_else(|| format!("no template receipt for `{actor}`"))?;
        let h = &receipt.actor_type_handle;
        if receipt.sil_template_hash != hash
            || h.template.hash != hash
            || h.template.prefix != prefix
            || h.template.suffix != suffix
            || !h.context_fields.is_empty()
        {
            return Err(format!("{actor}: template receipt or handle does not match {}", src.label));
        }
        let embedded = art.sil_abi.contracts.get(actor).ok_or_else(|| format!("artifact has no contract `{actor}`"))?;
        if types_of(&embedded.runtime_state.fields) != types_of(&hand.runtime_state.fields) {
            return Err(format!("{actor}: state layout differs from {}", src.label));
        }
        log.push(format!("{actor}: handle {} matches {}", hex(&hash), src.label));
    }
    Ok(log)
}

/// Check a published artifact against the silverc artifacts it claims to wrap, without argentc:
/// the id and internal consistency hold, and for every hand-written actor the embedded contract,
/// the template receipt, the actor-type handle and the interface fingerprint are exactly what
/// [`transplant`] derives from the silverc artifact. Returns one line per actor.
pub fn verify(art: &Artifact, sources: &[ActorSource]) -> Result<Vec<String>> {
    let log = verify_handles(art, sources)?;
    for src in sources {
        let actor = src.actor.as_str();
        let hand = hand_contract(src)?;
        let embedded = &art.sil_abi.contracts[actor];
        if embedded != hand {
            return Err(format!("{actor}: embedded contract differs from {}", src.label));
        }
        let actor_art = art.argent.actors.iter().find(|a| a.name == actor).ok_or_else(|| format!("no actor `{actor}`"))?;
        let fp = actor_interface_fingerprint_hex(actor, &actor_art.state, &hand.runtime_state.fields)
            .map_err(|e| format!("{actor}: fingerprint: {e}"))?;
        let export = art.argent.interfaces.exports.iter().find(|i| i.actor == actor).ok_or_else(|| format!("no export `{actor}`"))?;
        if export.fingerprint_hex != fp {
            return Err(format!("{actor}: exported interface fingerprint is stale"));
        }
    }
    Ok(log)
}

/// Check that every app `art` links (its `dependencies`) is one of the published `artifacts`, by
/// name and artifact id. Returns one line per dependency.
pub fn check_links(art: &Artifact, artifacts: &[Artifact]) -> Result<Vec<String>> {
    art.check_consistency().map_err(|e| format!("artifact is not consistent: {e}"))?;
    let mut log = Vec::new();
    for dep in &art.dependencies {
        let published = artifacts
            .iter()
            .find(|a| a.app == dep.app)
            .ok_or_else(|| format!("{} links app `{}`, which is not among the published artifacts", art.app, dep.app))?;
        if published.id != dep.artifact_id {
            return Err(format!("{} links {} {} but the published artifact is {}", art.app, dep.app, dep.artifact_id, published.id));
        }
        log.push(format!("{} links {} {}", art.app, dep.app, dep.artifact_id));
    }
    Ok(log)
}

/// argentc records absolute source paths (`root`, `modules`, contract `source_path`) in its
/// outputs. They are informational and excluded from the artifact id; make them relative to
/// `root` so committed output does not depend on the checkout. Handles `\`, `/` and the Windows
/// `\\?\` / `//?/` prefixes.
pub fn relativize_paths(text: &str, root: &Path) -> String {
    let plain = root.to_string_lossy().replace('\\', "/");
    let plain = plain.trim_start_matches("//?/").trim_end_matches('/').to_string();
    let mut out = text.to_string();
    for prefix in [format!("//?/{plain}/"), format!("{plain}/")] {
        out = out.replace(&prefix, "");
    }
    // JSON-escaped backslash forms.
    let esc = plain.replace('/', "\\\\");
    for prefix in [format!("\\\\\\\\?\\\\{esc}\\\\"), format!("{esc}\\\\")] {
        out = out.replace(&prefix, "");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relativize_handles_all_prefix_forms() {
        let root = Path::new("C:/Users/x/repo");
        let text = r#"{"a":"//?/C:/Users/x/repo/contracts/a.ag","b":"C:/Users/x/repo/b.ag","c":"C:\\Users\\x\\repo\\c.ag"}"#;
        assert_eq!(relativize_paths(text, root), r#"{"a":"contracts/a.ag","b":"b.ag","c":"c.ag"}"#);
        let unix = Path::new("/home/u/repo");
        assert_eq!(relativize_paths(r#"{"a":"/home/u/repo/x/y.ag"}"#, unix), r#"{"a":"x/y.ag"}"#);
    }
}

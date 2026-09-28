//! Saved rendering identity. Development labels migrate to the current policy;
//! valid captured assets stay pinned. Resolution never rewrites installed cubes.
use super::cube::CubeLut;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::Write,
    path::{Path, PathBuf},
};

pub const ENGINE_REVISION: &str = "v3-stable-input-2";
pub const INPUT_POLICY: &str = "profiled-display-cube-p3-or-compress-1";
/// Accepted development labels are normalized by migration before use. The old
/// bypass algorithm is not retained; all rendered inputs use the current policy.
const ACCEPTED_INPUT_POLICIES: &[&str] =
    &[INPUT_POLICY, "profiled-display-cube-or-wide-gamut-bypass-1"];
pub const RAW_REVISION: &str = "bayer-d65-green-clipped-neutral-1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    /// Hash of the original file bytes, not a filename supplied by an edit.
    pub blake3: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub schema: u32,
    pub engine: String,
    pub input_policy: String,
    pub raw_development: String,
    pub input_transform: Option<Asset>,
    pub output_transform: Option<Asset>,
    /// The Display P3 capture, when one was installed at pinning time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_transform_p3: Option<Asset>,
}

/// Effective pair used for an entire application render. A saved `None`
/// means no transform, not "use whatever is installed on this machine".
pub struct Resolved {
    pub input: Option<PathBuf>,
    pub input_p3: Option<PathBuf>,
    pub output: Option<PathBuf>,
    pub recovery: super::raw::Recovery,
}

impl Identity {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(self.schema == 1, "Unsupported v3 rendering-identity schema");
        ensure!(
            self.engine == ENGINE_REVISION || self.engine == "v3-stable-input-1",
            "This edit needs v3 renderer '{}'; this build provides '{}'. No substitute rendering was used.",
            self.engine,
            ENGINE_REVISION
        );
        ensure!(
            ACCEPTED_INPUT_POLICIES.contains(&self.input_policy.as_str())
                && self.raw_development == RAW_REVISION,
            "This edit needs an unsupported input or RAW-development revision"
        );
        for asset in [
            &self.input_transform,
            &self.output_transform,
            &self.input_transform_p3,
        ]
        .into_iter()
        .flatten()
        {
            ensure!(
                asset.blake3.len() == 64
                    && asset
                        .blake3
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "Invalid v3 transform digest"
            );
        }
        Ok(())
    }
}

fn checked_path(root: &Path, asset: &Asset) -> Result<PathBuf> {
    let path = root.join(format!("{}.cube", asset.blake3));
    let (digest, _) = super::file_version::digest(&path, false).with_context(|| format!(
        "Missing pinned v3 transform {}. Restore this asset from your backup; installed transforms will not be substituted.", asset.blake3))?;
    ensure!(
        digest == asset.blake3,
        "Pinned v3 transform {} has changed or is damaged. Restore the original asset.",
        asset.blake3
    );
    Ok(path)
}

pub fn resolve(state: &crate::AppState, edits: &Value) -> Result<Resolved> {
    let normalized = super::migration::normalize(edits)?;
    let edits = normalized.as_ref();
    let recovery = super::raw::Recovery::from_edits(edits)?;
    ensure!(
        recovery != super::raw::Recovery::Off
            || edits["v3Pipeline"]["engine"].as_str() == Some(ENGINE_REVISION),
        "Disabling RAW recovery requires the current pinned rendering revision. Use the RAW source option in the editor to adopt it safely."
    );
    let Some(value) = edits.get("v3Pipeline").filter(|v| !v.is_null()) else {
        // Unpinned edits use the current installed captures. Locking creates
        // immutable asset references; the renderer itself is always current v3.
        return Ok(Resolved {
            input: super::application::input_transform(state),
            input_p3: state.input_transform_p3.lock().ok().and_then(|p| p.clone()),
            output: super::application::output_transform(state),
            recovery,
        });
    };
    let identity: Identity =
        serde_json::from_value(value.clone()).context("Invalid saved v3 rendering identity")?;
    identity.validate()?;
    let root = state
        .v3_asset_dir
        .lock()
        .map_err(|_| anyhow::anyhow!("V3 asset store unavailable"))?
        .clone();
    let asset = |a: &Option<Asset>| -> Result<Option<PathBuf>> {
        a.as_ref()
            .map(|a| {
                checked_path(
                    root.as_deref()
                        .context("V3 transform store is unavailable")?,
                    a,
                )
            })
            .transpose()
    };
    Ok(Resolved {
        input: asset(&identity.input_transform)?,
        input_p3: asset(&identity.input_transform_p3)?,
        output: asset(&identity.output_transform)?,
        recovery,
    })
}

fn store(root: &Path, source: &Path) -> Result<Asset> {
    let bytes =
        std::fs::read(source).with_context(|| format!("Could not read {}", source.display()))?;
    CubeLut::parse(std::str::from_utf8(&bytes).context("Transform is not UTF-8")?)?;
    let asset = Asset {
        blake3: blake3::hash(&bytes).to_hex().to_string(),
    };
    std::fs::create_dir_all(root)?;
    let destination = root.join(format!("{}.cube", asset.blake3));
    if destination.exists() {
        checked_path(root, &asset)?;
        return Ok(asset);
    }
    // Publish atomically without overwriting a concurrent pin or existing asset.
    let mut temporary = tempfile::NamedTempFile::new_in(root)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(&destination) {
        Ok(_) => {}
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
            checked_path(root, &asset)?;
        }
        Err(e) => return Err(e.error.into()),
    }
    Ok(asset)
}

pub fn pin(state: &crate::AppState, edits: &Value) -> Result<Identity> {
    let normalized = super::migration::normalize(edits)?;
    let edits = normalized.as_ref();
    if let Some(value) = edits.get("v3Pipeline").filter(|v| !v.is_null()) {
        resolve(state, edits)?;
        return Ok(serde_json::from_value(value.clone())?);
    }
    let pair = resolve(state, edits)?;
    let root = state
        .v3_asset_dir
        .lock()
        .map_err(|_| anyhow::anyhow!("V3 asset store unavailable"))?
        .clone();
    let capture = |p: Option<PathBuf>| -> Result<Option<Asset>> {
        p.map(|p| {
            store(
                root.as_deref()
                    .context("V3 transform store is unavailable")?,
                &p,
            )
        })
        .transpose()
    };
    let identity = Identity {
        schema: 1,
        engine: ENGINE_REVISION.into(),
        input_policy: INPUT_POLICY.into(),
        raw_development: RAW_REVISION.into(),
        input_transform: capture(pair.input)?,
        output_transform: capture(pair.output)?,
        input_transform_p3: capture(pair.input_p3)?,
    };
    identity.validate()?;
    Ok(identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn development_revision_one_adopts_current_recovery_support() {
        let state = crate::AppState::default();
        let mut identity = pin(&state, &json!({})).unwrap();
        identity.engine = "v3-stable-input-1".into();
        assert!(resolve(&state, &json!({"v3Pipeline":identity})).is_ok());
        assert!(
            resolve(
                &state,
                &json!({"v3Pipeline":identity,"v3RawRecovery":"off"})
            )
            .is_ok()
        );
        identity.engine = ENGINE_REVISION.into();
        assert!(
            resolve(
                &state,
                &json!({"v3Pipeline":identity,"v3RawRecovery":"off"})
            )
            .is_ok()
        );
    }
    fn cube(v: f32) -> String {
        format!("LUT_3D_SIZE 2\n{}", format!("{v} {v} {v}\n").repeat(8))
    }

    #[test]
    fn pinned_pair_survives_installed_replacement_and_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let installed = dir.path().join("installed.cube");
        std::fs::write(&installed, cube(0.2)).unwrap();
        let state = crate::AppState::default();
        *state.v3_asset_dir.lock().unwrap() = Some(dir.path().join("assets"));
        *state.output_transform.lock().unwrap() = Some(installed.clone());
        let identity = pin(&state, &json!({})).unwrap();
        let saved = serde_json::to_string(&json!({"v3Pipeline":identity})).unwrap();
        std::fs::write(&installed, cube(0.4)).unwrap();
        let reopened: Value = serde_json::from_str(&saved).unwrap();
        let resolved = resolve(&state, &reopened).unwrap();
        assert!(resolved.input.is_none());
        assert_eq!(
            CubeLut::load(resolved.output.as_ref().unwrap())
                .unwrap()
                .sample([0.5; 3]),
            [0.2; 3]
        );
        assert_eq!(pin(&state, &reopened).unwrap(), identity);
        std::fs::write(resolved.output.unwrap(), cube(0.9)).unwrap();
        assert!(resolve(&state, &reopened).is_err());
    }

    #[test]
    fn missing_assets_unknown_revisions_and_invalid_digests_fail_closed() {
        let state = crate::AppState::default();
        let identity = pin(&state, &json!({})).unwrap();
        for bad in [
            json!({"v3Pipeline":{"schema":9}}),
            {
                let mut v = json!({"v3Pipeline":identity});
                v["v3Pipeline"]["engine"] = json!("future");
                v
            },
            {
                let mut v = json!({"v3Pipeline":identity});
                v["v3Pipeline"]["input_transform"] = json!({"blake3":"../../outside"});
                v
            },
            {
                let mut v = json!({"v3Pipeline":identity});
                v["v3Pipeline"]["output_transform"] = json!({"blake3":"a".repeat(64)});
                v
            },
        ] {
            assert!(resolve(&state, &bad).is_err());
        }
    }

    #[test]
    fn pinned_builtin_ignores_later_installs_and_old_edits_are_unchanged() {
        let state = crate::AppState::default();
        let edits = json!({"processVersion":3,"v3":{"exposure":0.5}});
        let before = edits.clone();
        let identity = pin(&state, &edits).unwrap();
        *state.output_transform.lock().unwrap() = Some(PathBuf::from("new.cube"));
        assert!(
            resolve(&state, &json!({"v3Pipeline":identity}))
                .unwrap()
                .output
                .is_none()
        );
        assert_eq!(
            resolve(&state, &edits).unwrap().output,
            Some(PathBuf::from("new.cube"))
        );
        assert_eq!(edits, before);
    }
}

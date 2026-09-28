//! Development-era edits adopt the sole supported renderer. No legacy look promise.
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::borrow::Cow;

pub fn normalize(edits: &Value) -> Result<Cow<'_, Value>> {
    ensure!(
        edits.is_null() || edits.is_object(),
        "Adjustments must be an object"
    );
    let version = edits.get("processVersion").filter(|v| !v.is_null());
    ensure!(
        version.is_none_or(|v| v.as_u64().is_some_and(|v| v <= 3)),
        "Unsupported future or invalid color engine version"
    );
    let old = version.and_then(Value::as_u64) != Some(3);
    let pipeline = &edits["v3Pipeline"];
    if !old
        && edits.get("v3PreviousVersion").is_none()
        && edits.get("v3PreviousToneMapper").is_none()
        && (pipeline.is_null()
            || (pipeline["engine"] == super::identity::ENGINE_REVISION
                && pipeline["input_policy"] == super::identity::INPUT_POLICY))
    {
        return Ok(Cow::Borrowed(edits));
    }
    let mut normalized = if edits.is_null() {
        json!({})
    } else {
        edits.clone()
    };
    normalized["processVersion"] = json!(3);
    if old {
        normalized["toneMapper"] = json!("resolve");
    }
    let obj = normalized.as_object_mut().unwrap();
    obj.remove("v3PreviousVersion");
    obj.remove("v3PreviousToneMapper");
    if let Some(value) = obj.get_mut("v3Pipeline").filter(|v| !v.is_null()) {
        let mut identity: super::identity::Identity = serde_json::from_value(value.clone())?;
        identity.validate()?;
        let changed = identity.engine != super::identity::ENGINE_REVISION
            || identity.input_policy != super::identity::INPUT_POLICY;
        identity.engine = super::identity::ENGINE_REVISION.into();
        identity.input_policy = super::identity::INPUT_POLICY.into();
        *value = serde_json::to_value(identity)?;
        if changed {
            obj.remove("v3Input");
        }
    }
    if old {
        obj.remove("v3Input");
    }
    if normalized == *edits {
        Ok(Cow::Borrowed(edits))
    } else {
        Ok(Cow::Owned(normalized))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn development_edits_adopt_v3_without_losing_shared_controls() {
        for version in [Value::Null, json!(1), json!(2), json!(3)] {
            let edits = json!({"processVersion":version,"shadows":73,"highlights":-61,"crop":{"x":4},"v3PreviousVersion":2});
            let result = normalize(&edits).unwrap();
            assert_eq!(result["processVersion"], 3);
            assert_eq!(result["shadows"], 73);
            assert_eq!(result["highlights"], -61);
            assert_eq!(result["crop"], edits["crop"]);
            assert!(result.get("v3PreviousVersion").is_none());
            assert_eq!(normalize(&result).unwrap().as_ref(), result.as_ref());
        }
        assert_eq!(normalize(&Value::Null).unwrap()["toneMapper"], "resolve");
        assert!(normalize(&json!({"processVersion":4})).is_err());
        assert!(normalize(&json!({"processVersion":"2"})).is_err());
    }
}

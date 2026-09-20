//! Partial edits preserve omitted fields. New environment values always become
//! immutable secret references before the configuration intent is persisted.
use crate::model::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use uuid::Uuid;

pub const EDIT_LIMIT: usize = 256 * 1024;

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceEdit {
    pub args: Option<Vec<String>>,
    pub cwd: Option<WorkingDirectory>,
    pub env: Option<EnvironmentEdit>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentEdit {
    #[serde(default)]
    pub set: Vec<EnvironmentAssignment>,
    #[serde(default)]
    pub unset: Vec<String>,
    #[serde(default)]
    pub inherit: Vec<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentAssignment {
    pub name: String,
    pub secret_id: Uuid,
    pub value: String,
}
impl InstanceEdit {
    pub(super) fn apply(&self, instance: &mut Instance) -> Result<(), ValidationError> {
        if self.args.is_none()
            && self.cwd.is_none()
            && self
                .env
                .as_ref()
                .is_none_or(|e| e.set.is_empty() && e.unset.is_empty() && e.inherit.is_empty())
        {
            return Err(ValidationError("EMPTY_INSTANCE_EDIT"));
        }
        if serde_json::to_vec(self)
            .map_err(|_| ValidationError("INVALID_INSTANCE_EDIT"))?
            .len()
            > EDIT_LIMIT
        {
            return Err(ValidationError("INSTANCE_EDIT_TOO_LARGE"));
        }
        if let Some(env) = &self.env {
            if env.set.len() + env.unset.len() + env.inherit.len() > 256 {
                return Err(ValidationError("INSTANCE_EDIT_TOO_LARGE"));
            }
            let mut names = HashSet::new();
            let mut ids = HashSet::new();
            let mut check = SavedEnvironment {
                set: BTreeMap::new(),
                unset: env.unset.iter().chain(&env.inherit).cloned().collect(),
            };
            for entry in &env.set {
                if entry.secret_id.is_nil() || !ids.insert(entry.secret_id) {
                    return Err(ValidationError("INVALID_SECRET_REFERENCE"));
                }
                if !names.insert(entry.name.to_ascii_uppercase()) {
                    return Err(ValidationError("INVALID_ENV_PATCH"));
                }
                check.set.insert(
                    entry.name.clone(),
                    EnvValue::Literal {
                        value: entry.value.clone(),
                    },
                );
            }
            validate_instance_input(&[], &check)?;
            for name in env
                .set
                .iter()
                .map(|e| &e.name)
                .chain(&env.unset)
                .chain(&env.inherit)
            {
                instance
                    .env
                    .set
                    .retain(|existing, _| !existing.eq_ignore_ascii_case(name));
                instance
                    .env
                    .unset
                    .retain(|existing| !existing.eq_ignore_ascii_case(name));
            }
            for entry in &env.set {
                instance.env.set.insert(
                    entry.name.clone(),
                    EnvValue::SecretRef {
                        id: entry.secret_id,
                    },
                );
            }
            instance.env.unset.extend(env.unset.iter().cloned());
        }
        if let Some(args) = &self.args {
            instance.args = args.clone();
        }
        if let Some(cwd) = &self.cwd {
            instance.cwd = cwd.clone();
        }
        // Data mode cannot be changed in place. Reject path placeholders that
        // have no meaning for this original instance before saving the edit.
        if matches!(instance.data, InstanceData::Original {}) {
            let check = |value: &str| {
                crate::model::expand_placeholders(value, |name| {
                    if name == "app_dir" {
                        Ok(String::new())
                    } else {
                        Err(ValidationError("ISOLATED_PATH_VARIABLE_IN_ORIGINAL"))
                    }
                })
                .map(|_| ())
            };
            for arg in &instance.args {
                check(arg)?;
            }
            if let WorkingDirectory::Explicit { path } = &instance.cwd {
                check(
                    path.to_str()
                        .ok_or(ValidationError("ABSOLUTE_UNICODE_PATH_REQUIRED"))?,
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

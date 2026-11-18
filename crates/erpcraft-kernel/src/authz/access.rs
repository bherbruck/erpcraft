//! What one actor could do, described for an interface.
//!
//! An interface uses this to decide what to offer: which screens, buttons,
//! columns and inputs. It is a courtesy and a coarse one. Every command
//! still makes the precise check, per record, when it runs.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Serialize;

use super::actor::{Actor, Reach};
use crate::registry::{Description, EntityMeta};

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Access {
    /// Per entity name.
    pub entities: BTreeMap<String, EntityAccess>,
    /// Per document type, the actions on it the actor could take, such as
    /// `read`, `create` or `post`.
    pub doc_types: BTreeMap<String, Vec<String>>,
    /// The commands the actor could run.
    pub commands: Vec<String>,
}

/// Whether the actor could act on at least some records of an entity.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EntityAccess {
    pub read: bool,
    pub create: bool,
    pub update: bool,
    pub delete: bool,
    /// The fields the actor cannot use freely, per action. A field not
    /// listed here is available on every record the actor can reach.
    pub fields: BTreeMap<String, FieldReach>,
}

/// `all`, `some` (depends on the record) or `none`, per action.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct FieldReach {
    pub read: Reach,
    pub create: Reach,
    pub update: Reach,
}

fn entity_access(actor: &Actor, entity: &EntityMeta) -> EntityAccess {
    let permission = |action: &str| format!("{}:{action}", entity.name);
    let (read, create, update) = (
        permission("read"),
        permission("create"),
        permission("update"),
    );
    let mut fields = BTreeMap::new();
    for field in &entity.fields {
        let reach = FieldReach {
            read: actor.field_reach(&read, &field.name),
            create: actor.field_reach(&create, &field.name),
            update: actor.field_reach(&update, &field.name),
        };
        if [reach.read, reach.create, reach.update] != [Reach::All; 3] {
            fields.insert(field.name.clone(), reach);
        }
    }
    let able = |permission: &str| actor.can_any(permission);
    let access = EntityAccess {
        read: able(&read),
        create: !entity.read_only && able(&create),
        update: !entity.read_only && able(&update),
        delete: !entity.read_only && able(&permission("delete")),
        fields,
    };
    // When an action is closed altogether, its fields say nothing more.
    if !(access.read || access.create || access.update || access.delete) {
        return EntityAccess {
            fields: BTreeMap::new(),
            ..access
        };
    }
    access
}

impl Access {
    /// The access `actor` has to what `description` lists.
    pub fn of(actor: &Actor, description: &Description) -> Access {
        // The actions of a document type are those its commands require,
        // plus the ones every document has.
        let mut doc_types = BTreeMap::new();
        for doc_type in &description.doc_types {
            let mut actions: Vec<String> = ["read", "create", "update", "delete"]
                .iter()
                .map(|a| a.to_string())
                .collect();
            for command in &description.commands {
                let action = command
                    .permission
                    .as_deref()
                    .and_then(|p| p.split_once(':'))
                    .filter(|(resource, _)| *resource == doc_type.name)
                    .map(|(_, action)| action.to_owned());
                if let Some(action) = action
                    && !actions.contains(&action)
                {
                    actions.push(action);
                }
            }
            actions.retain(|action| actor.can_any(&format!("{}:{action}", doc_type.name)));
            doc_types.insert(doc_type.name.clone(), actions);
        }

        Access {
            entities: description
                .entities
                .iter()
                .map(|entity| (entity.name.clone(), entity_access(actor, entity)))
                .collect(),
            doc_types,
            commands: description
                .commands
                .iter()
                .filter(|c| c.permission.as_deref().is_none_or(|p| actor.can_any(p)))
                .map(|c| c.name.clone())
                .collect(),
        }
    }
}

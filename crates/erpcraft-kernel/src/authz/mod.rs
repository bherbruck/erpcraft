//! Authorization, and the directory of users it applies to.
//!
//! The kernel knows who exists and what they may do. How someone proves who
//! they are (passwords, tokens, single sign-on) is not the kernel's
//! business: an [`Authenticator`] supplied by a module turns a credential
//! into an [`Actor`].
//!
//! - `actor`: the actor, its rules, and every permission check.
//! - `access`: what an actor could do, described for an interface.
//! - `directory`: the users, and loading an actor from their roles.

mod access;
mod actor;
mod directory;

pub use access::{Access, EntityAccess, FieldReach};
pub use actor::{Actor, Effect, FieldAccess, Grant, PermissionRule, Reach, RowScope, Target};
pub use directory::{Authenticator, User, create_user, load_actor};

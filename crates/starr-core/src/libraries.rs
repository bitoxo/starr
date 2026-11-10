//! Libraries per category (spec 0005 M4, slice 2): which libraries of a
//! consume instance belong to which category, and which libraries a member
//! gets there. Pure rules; the mapping is stored in the consume role's
//! settings (`starr-db`), the instance's library list comes from
//! `accounts.scan`.
//!
//! Rules:
//! - a library belongs to at most one category per instance, so an adult
//!   library can never also count as movies;
//! - a member gets an **explicit list**: the union of the libraries mapped
//!   to the categories the member is entitled to and that are switched on.
//!   There is no "all libraries", so a library added later never leaks to
//!   anyone; adult only with the member's adult entitlement.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::library::MediaLibrary;
use crate::members::{Category, Entitlement};

/// One library mapped to a category. Both IDs are kept: `id` is what item
/// queries and the inventory use, `grant_id` what account grants name
/// (Emby: the library `Guid`; Jellyfin: equal to `id`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MappedLibrary {
    pub id: String,
    pub grant_id: String,
}

/// Settings of a consume role: the instance's libraries that belong to the
/// role's category. Empty until the admin maps some.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsumeSettings {
    #[serde(default)]
    pub libraries: Vec<MappedLibrary>,
}

/// Why a library mapping was refused; [`MappingError::code`] for the API.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MappingError {
    #[error("library {0:?} is not in the instance's library list")]
    Unknown(String),
    #[error("library {library:?} already belongs to {category:?} on this instance")]
    Overlap { library: String, category: Category },
}

impl MappingError {
    /// Validation code for the API (ADR 0007).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unknown(_) => "categories.library_unknown",
            Self::Overlap { .. } => "categories.library_overlap",
        }
    }
}

/// The mapping of one consume role on an instance.
#[derive(Debug, Clone, Copy)]
pub struct RoleLibraries<'a> {
    pub category: Category,
    pub libraries: &'a [MappedLibrary],
}

/// Check the library IDs an admin picked for `category` on one instance and
/// return the mapping to store, in the order picked, without duplicates.
///
/// Each ID must be in the instance's last library list (`listed`) with a
/// grant ID, which the list supplies; an ID the role already maps but the list no
/// longer has (renamed in Jellyfin, removed, or missing from an incomplete
/// listing) may stay. An ID another category maps on the same instance
/// (`others`) is refused.
pub fn validate_mapping(
    category: Category,
    wanted: &[String],
    listed: &[MediaLibrary],
    current: &[MappedLibrary],
    others: &[RoleLibraries<'_>],
) -> Result<Vec<MappedLibrary>, MappingError> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for id in wanted {
        let id = id.trim();
        if !seen.insert(id) {
            continue;
        }
        let mapped = listed
            .iter()
            .find(|l| l.id == id)
            .and_then(|l| {
                Some(MappedLibrary {
                    id: l.id.clone(),
                    grant_id: l.grant_id.clone()?,
                })
            })
            .or_else(|| current.iter().find(|m| m.id == id).cloned())
            .ok_or_else(|| MappingError::Unknown(id.to_owned()))?;
        if let Some(other) = others
            .iter()
            .filter(|o| o.category != category)
            .find(|o| o.libraries.iter().any(|m| m.id == id))
        {
            return Err(MappingError::Overlap {
                library: id.to_owned(),
                category: other.category,
            });
        }
        out.push(mapped);
    }
    Ok(out)
}

/// Libraries of the instance that no consume role maps ("unmapped
/// library" under "needs you"), in listing order.
#[must_use]
pub fn unmapped<'a>(
    listed: &'a [MediaLibrary],
    roles: &[RoleLibraries<'_>],
) -> Vec<&'a MediaLibrary> {
    let mapped: HashSet<&str> = roles
        .iter()
        .flat_map(|r| r.libraries.iter().map(|m| m.id.as_str()))
        .collect();
    listed
        .iter()
        .filter(|l| !mapped.contains(l.id.as_str()))
        .collect()
}

/// Whether a member with `entitlements` gets `category`: it is switched on
/// (`enabled`) and the member's entitlement for it is enabled. A missing
/// entitlement counts as off, so adult needs an explicit one.
#[must_use]
pub fn entitled(entitlements: &[Entitlement], enabled: &[Category], category: Category) -> bool {
    enabled.contains(&category)
        && entitlements
            .iter()
            .rev()
            .find(|e| e.category == category)
            .is_some_and(|e| e.enabled)
}

/// The libraries a member gets on one instance: the union of the
/// libraries mapped by the instance's consume roles (`roles`) whose
/// category is switched on (`enabled`) and enabled in the member's
/// `entitlements` (a missing entitlement counts as off, so adult needs an
/// explicit one). Sorted by grant ID, each once. Empty means the member
/// has no category there; it never means "all".
#[must_use]
pub fn desired(
    entitlements: &[Entitlement],
    enabled: &[Category],
    roles: &[RoleLibraries<'_>],
) -> Vec<MappedLibrary> {
    let mut by_grant: BTreeMap<&str, &MappedLibrary> = BTreeMap::new();
    for role in roles
        .iter()
        .filter(|r| entitled(entitlements, enabled, r.category))
    {
        for library in role.libraries {
            by_grant.entry(library.grant_id.as_str()).or_insert(library);
        }
    }
    by_grant.into_values().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::LibraryContent;
    use crate::members::RequestMode;

    fn lib(id: &str, grant: &str) -> MappedLibrary {
        MappedLibrary {
            id: id.to_owned(),
            grant_id: grant.to_owned(),
        }
    }

    fn listed(id: &str, grant: &str, name: &str) -> MediaLibrary {
        MediaLibrary {
            id: id.to_owned(),
            grant_id: Some(grant.to_owned()),
            name: name.to_owned(),
            content: LibraryContent::Movies,
        }
    }

    fn ent(category: Category, enabled: bool) -> Entitlement {
        Entitlement {
            category,
            enabled,
            request_mode: RequestMode::Approval,
        }
    }

    /// An Emby with movies, series and an adult library.
    fn emby() -> Vec<MediaLibrary> {
        vec![
            listed("3", "g-movies", "Movies"),
            listed("6", "g-tv", "TV"),
            listed("12", "g-adult", "Adult"),
            listed("15", "g-home", "Home videos"),
        ]
    }

    #[test]
    fn a_mapping_takes_grant_ids_from_the_listing_and_drops_duplicates() {
        let got = validate_mapping(
            Category::Movies,
            &["3".into(), " 3".into()],
            &emby(),
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(got, vec![lib("3", "g-movies")]);
    }

    #[test]
    fn unknown_libraries_are_refused_but_stale_mapped_ones_may_stay() {
        assert_eq!(
            validate_mapping(Category::Movies, &["99".into()], &emby(), &[], &[]),
            Err(MappingError::Unknown("99".into()))
        );
        // "old" was mapped before and is no longer listed: kept as it is.
        let current = [lib("old", "g-old")];
        let got = validate_mapping(
            Category::Movies,
            &["old".into(), "3".into()],
            &emby(),
            &current,
            &[],
        )
        .unwrap();
        assert_eq!(got, vec![lib("old", "g-old"), lib("3", "g-movies")]);
    }

    #[test]
    fn libraries_without_a_grant_id_cannot_be_mapped() {
        let list = [MediaLibrary {
            grant_id: None,
            ..listed("3", "x", "Movies")
        }];
        assert_eq!(
            validate_mapping(Category::Movies, &["3".into()], &list, &[], &[]),
            Err(MappingError::Unknown("3".into()))
        );
    }

    #[test]
    fn a_library_belongs_to_at_most_one_category_per_instance() {
        let adult = [lib("12", "g-adult")];
        let others = [RoleLibraries {
            category: Category::Adult,
            libraries: &adult,
        }];
        let err = validate_mapping(
            Category::Movies,
            &["3".into(), "12".into()],
            &emby(),
            &[],
            &others,
        )
        .unwrap_err();
        assert_eq!(
            err,
            MappingError::Overlap {
                library: "12".into(),
                category: Category::Adult,
            }
        );
        assert_eq!(err.code(), "categories.library_overlap");
        // The role's own category is not an overlap (re-saving).
        let own = [RoleLibraries {
            category: Category::Movies,
            libraries: &adult,
        }];
        assert!(validate_mapping(Category::Movies, &["12".into()], &emby(), &[], &own).is_ok());
    }

    #[test]
    fn libraries_no_role_maps_are_unmapped() {
        let movies = [lib("3", "g-movies")];
        let adult = [lib("12", "g-adult")];
        let list = emby();
        let got: Vec<&str> = unmapped(
            &list,
            &[
                RoleLibraries {
                    category: Category::Movies,
                    libraries: &movies,
                },
                RoleLibraries {
                    category: Category::Adult,
                    libraries: &adult,
                },
            ],
        )
        .iter()
        .map(|l| l.name.as_str())
        .collect();
        assert_eq!(got, vec!["TV", "Home videos"]);
    }

    /// Two members on one Emby: Alex has movies and series, Sam also adult.
    #[test]
    fn members_get_exactly_their_categories_libraries_and_adult_only_with_the_entitlement() {
        let movies = [lib("3", "g-movies")];
        let series = [lib("6", "g-tv")];
        let adult = [lib("12", "g-adult")];
        let roles = [
            RoleLibraries {
                category: Category::Movies,
                libraries: &movies,
            },
            RoleLibraries {
                category: Category::Series,
                libraries: &series,
            },
            RoleLibraries {
                category: Category::Adult,
                libraries: &adult,
            },
        ];
        let all_on = Category::ALL;
        let alex = [
            ent(Category::Movies, true),
            ent(Category::Series, true),
            ent(Category::Adult, false),
        ];
        let sam = [
            ent(Category::Movies, true),
            ent(Category::Series, true),
            ent(Category::Adult, true),
        ];
        let grants = |e: &[Entitlement], enabled: &[Category]| -> Vec<String> {
            desired(e, enabled, &roles)
                .into_iter()
                .map(|l| l.grant_id)
                .collect()
        };
        assert_eq!(grants(&alex, &all_on), vec!["g-movies", "g-tv"]);
        assert_eq!(grants(&sam, &all_on), vec!["g-adult", "g-movies", "g-tv"]);
        // Adult switched off as a category: nobody gets it.
        let no_adult = [Category::Movies, Category::Series];
        assert_eq!(grants(&sam, &no_adult), vec!["g-movies", "g-tv"]);
        // No stored entitlement counts as off: adult is never a default.
        assert_eq!(
            grants(&[ent(Category::Movies, true)], &all_on),
            vec!["g-movies"]
        );
        // Nothing entitled: an empty list, never "all".
        assert_eq!(grants(&[], &all_on), Vec::<String>::new());
        // The home videos library is mapped by nobody, so nobody gets it.
        assert!(!grants(&sam, &all_on).contains(&"g-home".to_owned()));
    }

    #[test]
    fn a_library_reached_through_two_roles_is_granted_once() {
        let shared = [lib("3", "g-movies")];
        let roles = [
            RoleLibraries {
                category: Category::Movies,
                libraries: &shared,
            },
            RoleLibraries {
                category: Category::Series,
                libraries: &shared,
            },
        ];
        let e = [ent(Category::Movies, true), ent(Category::Series, true)];
        assert_eq!(
            desired(&e, &Category::ALL, &roles),
            vec![lib("3", "g-movies")]
        );
    }

    #[test]
    fn consume_settings_read_tolerantly() {
        let s: ConsumeSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.libraries, vec![]);
        let s: ConsumeSettings =
            serde_json::from_str(r#"{"libraries":[{"id":"3","grantId":"g"}],"later":1}"#).unwrap();
        assert_eq!(s.libraries, vec![lib("3", "g")]);
    }
}

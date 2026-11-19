//! Structured view of PICS app/package info and DepotDownloader-style depot selection.

use std::collections::{BTreeSet, HashMap};

use super::backend::Ownership;
use super::kv::Kv;

pub const PUBLIC_BRANCH: &str = "public";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppInfo {
    pub app_id: u32,
    pub name: String,
    /// `common.type`, lowercased (`game`, `dlc`, `tool`, ...).
    pub app_type: String,
    pub free_to_download: bool,
    /// `depots.branches.public.buildid`.
    pub public_buildid: Option<String>,
    pub depots: Vec<DepotInfo>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DepotInfo {
    pub depot_id: u32,
    pub oslist: Option<String>,
    pub osarch: Option<String>,
    pub language: Option<String>,
    pub lowviolence: bool,
    /// The depot has a `manifests` section (possibly without a public entry).
    pub has_manifests: bool,
    pub public_manifest: Option<u64>,
    /// `manifests.public.size`, falling back to `maxsize`.
    pub size: Option<u64>,
    pub depot_from_app: Option<u32>,
}

/// A depot chosen for prefill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepotPlan {
    pub depot_id: u32,
    pub manifest_gid: u64,
    /// App id used for the manifest request code (the `depotfromapp` app for shared depots).
    pub containing_app: u32,
}

/// Builds an [`AppInfo`] from the PICS text KeyValues root (`"appinfo" { ... }`).
pub fn parse_app_info(app_id: u32, kv: &Kv) -> AppInfo {
    let depots_kv = kv.get("depots");
    let depots = depots_kv
        .map(|d| d.children())
        .unwrap_or_default()
        .iter()
        .filter_map(|(name, depot)| parse_depot(name, depot))
        .collect();
    AppInfo {
        app_id,
        name: kv
            .str_at(&["common", "name"])
            .unwrap_or_default()
            .to_string(),
        app_type: kv
            .str_at(&["common", "type"])
            .unwrap_or_default()
            .to_ascii_lowercase(),
        free_to_download: kv_bool(kv.str_at(&["common", "freetodownload"])),
        public_buildid: kv
            .str_at(&["depots", "branches", PUBLIC_BRANCH, "buildid"])
            .map(str::to_string),
        depots,
    }
}

fn parse_depot(name: &str, depot: &Kv) -> Option<DepotInfo> {
    let depot_id = name.parse().ok()?;
    if depot.children().is_empty() {
        return None;
    }
    let config = |k: &str| {
        depot
            .str_at(&["config", k])
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let manifests = depot.get("manifests");
    let public = manifests.and_then(|m| m.get(PUBLIC_BRANCH));
    // Current format: public { gid size download }; legacy: "public" "<gid>".
    let gid = public.and_then(|p| p.as_str().or_else(|| p.str_at(&["gid"])));
    let size = public
        .and_then(|p| p.str_at(&["size"]))
        .or_else(|| depot.str_at(&["maxsize"]))
        .and_then(|s| s.parse().ok());
    Some(DepotInfo {
        depot_id,
        oslist: config("oslist"),
        osarch: config("osarch"),
        language: config("language"),
        lowviolence: kv_bool(depot.str_at(&["config", "lowviolence"])),
        has_manifests: manifests.is_some(),
        public_manifest: gid.and_then(|g| g.parse().ok()),
        size,
        depot_from_app: depot.str_at(&["depotfromapp"]).and_then(|s| s.parse().ok()),
    })
}

fn kv_bool(v: Option<&str>) -> bool {
    matches!(v.map(str::trim), Some("1") | Some("true"))
}

/// Package `appids` and `depotids` from a PICS package KeyValues root.
pub fn package_ids(kv: &Kv) -> (Vec<u32>, Vec<u32>) {
    let ids = |key: &str| {
        kv.get(key)
            .map(|k| k.children())
            .unwrap_or_default()
            .iter()
            .filter_map(|(_, v)| v.as_str()?.parse().ok())
            .collect()
    };
    (ids("appids"), ids("depotids"))
}

/// Depots a Windows x64 English client would install (DepotDownloader defaults)
/// that the account can access.
pub fn filter_depots<'a>(app: &'a AppInfo, owned: &Ownership) -> Vec<&'a DepotInfo> {
    app.depots
        .iter()
        .filter(|d| {
            let os_ok = d
                .oslist
                .as_deref()
                .is_none_or(|l| l.split(',').any(|os| os.trim() == "windows"));
            let arch_ok = d.osarch.as_deref().is_none_or(|a| a == "64");
            let lang_ok = d.language.as_deref().is_none_or(|l| l == "english");
            os_ok && arch_ok && lang_ok && !d.lowviolence
        })
        .filter(|d| app.free_to_download || owned.has(d.depot_id))
        .collect()
}

/// Shared-install apps whose info is needed to resolve the given depots' manifests.
pub fn proxy_apps(app: &AppInfo, depots: &[&DepotInfo]) -> Vec<u32> {
    let set: BTreeSet<u32> = depots
        .iter()
        .filter(|d| !d.has_manifests)
        .filter_map(|d| d.depot_from_app)
        .filter(|&id| id != app.app_id)
        .collect();
    set.into_iter().collect()
}

/// Resolves the public manifest of each depot (following `depotfromapp` into
/// `proxies` when the depot has no manifests of its own). Returns the plans and
/// notes about depots that were skipped.
pub fn resolve_plans(
    app: &AppInfo,
    depots: &[&DepotInfo],
    proxies: &HashMap<u32, AppInfo>,
) -> (Vec<DepotPlan>, Vec<String>) {
    let mut plans = Vec::new();
    let mut notes = Vec::new();
    for depot in depots {
        let source = match (depot.has_manifests, depot.depot_from_app) {
            (false, Some(proxy)) if proxy != app.app_id => proxies
                .get(&proxy)
                .and_then(|p| p.depots.iter().find(|d| d.depot_id == depot.depot_id)),
            _ => Some(*depot),
        };
        let Some(gid) = source.and_then(|d| d.public_manifest) else {
            notes.push(format!(
                "depot {} has no public manifest; skipped",
                depot.depot_id
            ));
            continue;
        };
        let containing_app = match depot.depot_from_app {
            Some(proxy) if !app.free_to_download => proxy,
            _ => app.app_id,
        };
        plans.push(DepotPlan {
            depot_id: depot.depot_id,
            manifest_gid: gid,
            containing_app,
        });
    }
    (plans, notes)
}

#[cfg(test)]
mod tests {
    use super::super::kv::parse_text;
    use super::*;

    const APP: &str = r#""appinfo"
{
    "appid" "100"
    "common" { "name" "Test Game" "type" "Game" }
    "depots"
    {
        "101" { "config" { "oslist" "windows" } "manifests" { "public" { "gid" "1001" "size" "500" } } }
        "102" { "config" { "oslist" "macos" } "manifests" { "public" { "gid" "1002" } } }
        "103" { "config" { "oslist" "windows,linux" "osarch" "32" } "manifests" { "public" { "gid" "1003" } } }
        "104" { "config" { "language" "german" } "manifests" { "public" { "gid" "1004" } } }
        "105" { "config" { "language" "english" "osarch" "64" } "manifests" { "public" "1005" } "maxsize" "77" }
        "106" { "config" { "lowviolence" "1" } "manifests" { "public" { "gid" "1006" } } }
        "107" { "manifests" { "beta" { "gid" "1007" } } }
        "108" { "manifests" { "public" { "gid" "1008" } } }
        "109" { "depotfromapp" "200" "sharedinstall" "1" }
        "110" { "config" { "oslist" "" } "manifests" { "public" { "gid" "1010" } } "dlcappid" "150" }
        "branches" { "public" { "buildid" "555" } "beta" { "buildid" "556" } }
        "baselanguages" "english"
    }
}"#;

    fn app() -> AppInfo {
        parse_app_info(100, &parse_text(APP).unwrap().1)
    }

    fn owned(ids: &[u32]) -> Ownership {
        Ownership {
            app_ids: [100].into(),
            depot_ids: ids.iter().copied().collect(),
        }
    }

    #[test]
    fn parses_app_info() {
        let a = app();
        assert_eq!(a.name, "Test Game");
        assert_eq!(a.app_type, "game");
        assert_eq!(a.public_buildid.as_deref(), Some("555"));
        assert_eq!(a.depots.len(), 10);
        let d101 = &a.depots[0];
        assert_eq!(d101.public_manifest, Some(1001));
        assert_eq!(d101.size, Some(500));
        let d105 = a.depots.iter().find(|d| d.depot_id == 105).unwrap();
        assert_eq!(d105.public_manifest, Some(1005)); // legacy format
        assert_eq!(d105.size, Some(77));
        let d109 = a.depots.iter().find(|d| d.depot_id == 109).unwrap();
        assert!(!d109.has_manifests);
        assert_eq!(d109.depot_from_app, Some(200));
        assert!(
            a.depots
                .iter()
                .find(|d| d.depot_id == 110)
                .unwrap()
                .oslist
                .is_none()
        );
    }

    #[test]
    fn depot_selection_filters() {
        let a = app();
        let all: Vec<u32> = (101..=110).collect();
        let picked: Vec<u32> = filter_depots(&a, &owned(&all))
            .iter()
            .map(|d| d.depot_id)
            .collect();
        // 102 macos, 103 32-bit, 104 german, 106 low violence are dropped
        assert_eq!(picked, vec![101, 105, 107, 108, 109, 110]);

        // unowned depots (e.g. DLC 110) are dropped unless the app is free to download
        let picked: Vec<u32> = filter_depots(&a, &owned(&[101, 108]))
            .iter()
            .map(|d| d.depot_id)
            .collect();
        assert_eq!(picked, vec![101, 108]);
        let mut free = a.clone();
        free.free_to_download = true;
        assert_eq!(filter_depots(&free, &Ownership::default()).len(), 6);
    }

    #[test]
    fn plans_follow_depotfromapp_and_skip_missing() {
        let a = app();
        let all: Vec<u32> = (101..=110).collect();
        let owned = owned(&all);
        let depots = filter_depots(&a, &owned);
        assert_eq!(proxy_apps(&a, &depots), vec![200]);

        let proxy = AppInfo {
            app_id: 200,
            depots: vec![DepotInfo {
                depot_id: 109,
                has_manifests: true,
                public_manifest: Some(2009),
                ..Default::default()
            }],
            ..Default::default()
        };
        let (plans, notes) = resolve_plans(&a, &depots, &HashMap::from([(200, proxy)]));
        let got: Vec<(u32, u64, u32)> = plans
            .iter()
            .map(|p| (p.depot_id, p.manifest_gid, p.containing_app))
            .collect();
        assert_eq!(
            got,
            vec![
                (101, 1001, 100),
                (105, 1005, 100),
                (108, 1008, 100),
                (109, 2009, 200),
                (110, 1010, 100)
            ]
        );
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("107"));

        // without the proxy app's info the shared depot is skipped with a note
        let (plans, notes) = resolve_plans(&a, &depots, &HashMap::new());
        assert_eq!(plans.len(), 4);
        assert_eq!(notes.len(), 2);
    }

    #[test]
    fn package_appids_and_depotids() {
        let kv = parse_text(
            r#""42" { "packageid" "42" "appids" { "0" "730" "1" "740" } "depotids" { "0" "731" "1" "x" } }"#,
        )
        .unwrap()
        .1;
        assert_eq!(package_ids(&kv), (vec![730, 740], vec![731]));
        assert_eq!(package_ids(&Kv::Obj(vec![])), (vec![], vec![]));
    }
}

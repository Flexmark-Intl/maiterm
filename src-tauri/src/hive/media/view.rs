//! `hive_media_catalogue`: the catalogue as the media panel draws it (docs/hive-media.md §7).
//!
//! `hive_media_status` answers with ids only; the panel also needs each provider's display facts
//! (name, credits line inputs, guide, links). This derives them from `media.rs`, so the UI never
//! parses `catalogue.json` itself. The catalogue holds no secret, and this exposes only what the
//! panel shows: not the hosts (§9's sandbox hosts are no one's business in the UI), not the
//! detection recipes, not `source_urls`.
//!
//! Every kind is a String (`route_kind`, `auth`, `credits`), never an enum: the catalogue grows
//! kinds, and an older panel must show an unknown one rather than fail to parse.

use serde::Serialize;

use super::{catalogue, Provider};

/// A capability as the panel names it, in catalogue (display) order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapabilityView {
    pub id: String,
    /// The catalogue's `name`: "Video", "Images", "Finishing".
    pub label: String,
    pub means: String,
    /// One of the common five the first page shows; the rest sit under "More".
    pub common: bool,
}

/// A provider as the panel shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderView {
    pub id: String,
    pub name: String,
    pub capabilities: Vec<String>,
    pub models_note: String,
    /// What a job spends, from the provider's main route: `api` | `plan` | `free` | `plan_or_wallet`
    /// | `comfy_credits`, or "" for a provider with no route. The wording is the UI's
    /// (`creditsLine`), in maiTerm's words, never the provider's.
    pub credits: String,
    /// The main route's kind: `remote_mcp` | `brew` | `plugin` | `local_app` | `manual`, or "".
    pub route_kind: String,
    /// The main route's sign-in: `oauth` | `token` | `none`, or "".
    pub auth: String,
    /// A tool that prices a job before it runs, if the main route has one ("" when none).
    pub quote_tool: String,
    /// How long a result URL lives, if known: the brief's "save at once".
    pub retention: String,
    pub signup_url: String,
    pub billing_url: String,
    /// The steps only the human can do.
    pub guide: Vec<String>,
    /// D1: shown first to someone who already pays for it.
    pub subscriber: bool,
    /// When its facts were last checked (a date, `YYYY-MM-DD`): providers change monthly (§2.5).
    pub checked: String,
    /// Something about it nobody has confirmed from a real job: a host, its route, or a model
    /// claim. The panel marks it so; `unverified_capabilities` says which capabilities.
    pub unverified: bool,
    /// Capabilities whose claim for this provider is marked UNVERIFIED in its notes.
    pub unverified_capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogueView {
    pub capabilities: Vec<CapabilityView>,
    pub providers: Vec<ProviderView>,
}

/// The route the panel describes a provider by: its first remote MCP route (the one setup uses),
/// else its first route of any kind.
fn main_route(p: &Provider) -> Option<&super::Route> {
    p.mcp_route().or_else(|| p.routes.first())
}

fn provider_view(p: &Provider) -> ProviderView {
    let route = main_route(p);
    let mut unverified_capabilities: Vec<String> = p
        .capability_notes
        .iter()
        .filter(|(_, note)| note.trim_start().to_ascii_lowercase().starts_with("unverified"))
        .map(|(cap, _)| cap.clone())
        .collect();
    // HashMap order is not display order: follow the provider's own capability list.
    unverified_capabilities.sort_by_key(|c| p.capabilities.iter().position(|k| k == c).unwrap_or(usize::MAX));
    let unverified = !p.unverified_hosts.is_empty() || p.models_note.to_ascii_lowercase().contains("unverified") || !unverified_capabilities.is_empty();
    ProviderView {
        id: p.id.clone(),
        name: p.name.clone(),
        capabilities: p.capabilities.clone(),
        models_note: p.models_note.clone(),
        credits: route.map(|r| r.credits.clone()).unwrap_or_default(),
        route_kind: route.map(|r| r.kind.clone()).unwrap_or_default(),
        auth: route.map(|r| r.auth.clone()).unwrap_or_default(),
        quote_tool: route.and_then(|r| r.quote_tool.clone()).unwrap_or_default(),
        retention: p.retention.clone(),
        signup_url: p.signup_url.clone(),
        billing_url: p.billing_url.clone(),
        guide: p.guide.clone(),
        subscriber: p.subscriber_first,
        checked: p.checked.clone(),
        unverified,
        unverified_capabilities,
    }
}

/// The whole catalogue as the panel shows it. Pure: a function of the shipped file.
pub fn view() -> CatalogueView {
    let c = catalogue();
    CatalogueView {
        capabilities: c.capabilities.iter().map(|k| CapabilityView { id: k.id.clone(), label: k.name.clone(), means: k.means.clone(), common: k.common }).collect(),
        providers: c.providers.iter().map(provider_view).collect(),
    }
}

/// The UI's view of the media catalogue (capabilities in display order, providers with their
/// display facts). No arguments and no I/O: the catalogue is compiled in.
#[tauri::command]
pub fn hive_media_catalogue() -> CatalogueView {
    view()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p<'a>(v: &'a CatalogueView, id: &str) -> &'a ProviderView {
        v.providers.iter().find(|p| p.id == id).unwrap_or_else(|| panic!("provider {id}"))
    }

    #[test]
    fn capabilities_come_in_catalogue_order_with_the_common_five_first() {
        let v = view();
        assert_eq!(v.capabilities.len(), catalogue().capabilities.len());
        assert_eq!(v.capabilities.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), catalogue().capabilities.iter().map(|c| c.id.as_str()).collect::<Vec<_>>());
        let common: Vec<&str> = v.capabilities.iter().filter(|c| c.common).map(|c| c.id.as_str()).collect();
        assert_eq!(common, ["video", "image", "voice", "music", "finish"]);
        let video = &v.capabilities[0];
        assert_eq!((video.label.as_str(), video.means.as_str()), ("Video", "Text or image to video"));
    }

    #[test]
    fn every_catalogue_provider_is_there_with_its_facts() {
        let v = view();
        assert_eq!(v.providers.len(), catalogue().providers.len());
        for (view, p) in v.providers.iter().zip(&catalogue().providers) {
            assert_eq!(view.id, p.id, "catalogue order");
            assert_eq!(view.name, p.name);
            assert_eq!(view.capabilities, p.capabilities);
            assert_eq!(view.guide, p.guide);
            assert_eq!(view.checked, p.checked);
            assert_eq!(view.subscriber, p.subscriber_first);
        }
    }

    #[test]
    fn a_remote_mcp_provider_reads_its_route() {
        let v = view();
        let fal = p(&v, "fal");
        assert_eq!((fal.route_kind.as_str(), fal.auth.as_str(), fal.credits.as_str(), fal.quote_tool.as_str()), ("remote_mcp", "oauth", "api", "get_pricing"));
        assert_eq!(fal.retention, "7 days or longer");
        assert!(!fal.subscriber);
        let hf = p(&v, "higgsfield");
        assert_eq!((hf.credits.as_str(), hf.quote_tool.as_str()), ("plan", ""));
        assert!(hf.subscriber, "D1: Higgsfield shows first to someone who already subscribes");
    }

    #[test]
    fn a_free_local_provider_has_no_quote_and_costs_nothing() {
        let v = view();
        let kit = p(&v, "homebrew-kit");
        assert_eq!((kit.route_kind.as_str(), kit.auth.as_str(), kit.credits.as_str(), kit.quote_tool.as_str()), ("brew", "none", "free", ""));
    }

    #[test]
    fn unverified_is_flagged_for_a_note_a_route_or_a_host() {
        let v = view();
        let fal = p(&v, "fal");
        assert!(fal.unverified, "fal's music and sfx models are marked UNVERIFIED");
        assert_eq!(fal.unverified_capabilities, ["music", "sfx"], "in the provider's own capability order");
        assert!(p(&v, "bfl").unverified, "its route's path is unverified");
        assert!(!p(&v, "homebrew-kit").unverified);
        assert!(p(&v, "homebrew-kit").unverified_capabilities.is_empty());
    }

    #[test]
    fn a_provider_with_no_route_answers_empty_strings_not_a_panic() {
        let mut bare = catalogue().providers[0].clone();
        bare.routes.clear();
        let pv = provider_view(&bare);
        assert_eq!((pv.route_kind.as_str(), pv.auth.as_str(), pv.credits.as_str(), pv.quote_tool.as_str()), ("", "", "", ""));
    }

    #[test]
    fn the_wire_shape_is_plain_json_strings_and_arrays() {
        let json = serde_json::to_value(view()).unwrap();
        let fal = json["providers"].as_array().unwrap().iter().find(|p| p["id"] == "fal").unwrap();
        for k in ["id", "name", "models_note", "credits", "route_kind", "auth", "quote_tool", "retention", "signup_url", "billing_url", "checked"] {
            assert!(fal[k].is_string(), "{k}");
        }
        for k in ["capabilities", "guide", "unverified_capabilities"] {
            assert!(fal[k].is_array(), "{k}");
        }
        for k in ["subscriber", "unverified"] {
            assert!(fal[k].is_boolean(), "{k}");
        }
        assert!(json["capabilities"][0]["common"].is_boolean());
    }

    #[test]
    fn nothing_the_panel_does_not_show_leaves_rust() {
        // Hosts, detection recipes and source urls stay out of the wire shape.
        let text = serde_json::to_string(&view()).unwrap();
        for k in ["sandbox_hosts", "api_hosts", "result_hosts", "unverified_hosts", "source_urls", "detect", "match"] {
            assert!(!text.contains(&format!("\"{k}\"")), "{k}");
        }
    }
}

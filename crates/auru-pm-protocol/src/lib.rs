//! Transport types shared by Auru PM clients and servers.

use serde::{Deserialize, Serialize};

/// HTTP protocol version implemented by this workspace.
pub const WIRE_VERSION: &str = "auru-pm-v1";

/// Response returned by `GET /v1/health`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthResponse<C> {
    pub protocol: String,
    /// Stable identifier used in commit author identities and sidecars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub capabilities: C,
    /// Standards-based OAuth/OIDC settings safe to publish to desktop clients.
    ///
    /// Absent for unauthenticated providers and legacy providers whose
    /// authentication flow is described only by `capabilities.auth_methods`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication: Option<OAuthClientConfiguration>,
}

/// Public OAuth configuration for one PM server.
///
/// Endpoint URLs are deliberately absent: clients discover them from the
/// issuer's RFC 8414 / OpenID Connect metadata instead of trusting duplicated
/// configuration.
///
/// A provider may register more than one public client, because a desktop app
/// and a browser dashboard cannot share one. A native client redirects to an
/// exact loopback URI; a single-page app redirects to an `https` URL it serves
/// itself. Identity providers treat those as separate registrations, so
/// [`clients`](Self::clients) is a list and each entry names its
/// [`OAuthClientKind`].
///
/// The singular `client_id` / `redirect_uri` / `flows` fields predate that list
/// and describe the native client. They are still written, so a client built
/// before `clients` existed keeps working, and still read, so a provider that
/// publishes only the list is understood: whichever form arrives, both are
/// populated after deserialization.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(from = "OAuthClientConfigurationWire")]
pub struct OAuthClientConfiguration {
    pub issuer: String,
    pub audience: String,
    pub required_scope: String,
    /// Native client id. Equal to the [`OAuthClientKind::Native`] entry of
    /// [`clients`](Self::clients).
    pub client_id: String,
    /// Native client redirect. An exact loopback URI.
    pub redirect_uri: String,
    /// Flows permitted for the native client.
    pub flows: Vec<OAuthFlow>,
    /// Every public client this provider has registered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clients: Vec<OAuthClient>,
}

impl OAuthClientConfiguration {
    /// Build from the registered clients, deriving the compatibility fields.
    ///
    /// The only constructor worth using: building the struct literally makes it
    /// possible to publish a `clients` list and singular fields that disagree,
    /// which readers on either side of the change would resolve differently.
    /// This runs the same normalization as deserialization, so there is exactly
    /// one definition of the consistent form.
    pub fn new(
        issuer: impl Into<String>,
        audience: impl Into<String>,
        required_scope: impl Into<String>,
        clients: Vec<OAuthClient>,
    ) -> Self {
        OAuthClientConfigurationWire {
            issuer: issuer.into(),
            audience: audience.into(),
            required_scope: required_scope.into(),
            client_id: None,
            redirect_uri: None,
            flows: None,
            clients,
        }
        .into()
    }

    /// The client registration matching `kind`, if the provider published one.
    pub fn client(&self, kind: OAuthClientKind) -> Option<&OAuthClient> {
        self.clients.iter().find(|client| client.kind == kind)
    }

    /// The browser (single-page app) client, if this provider supports one.
    ///
    /// Absent is the normal case: a provider only serving a desktop app has no
    /// reason to register a second client.
    pub fn browser_client(&self) -> Option<&OAuthClient> {
        self.client(OAuthClientKind::Browser)
    }
}

/// One public client registered with the provider's identity provider.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthClient {
    pub kind: OAuthClientKind,
    pub client_id: String,
    pub redirect_uri: String,
    pub flows: Vec<OAuthFlow>,
}

/// Which kind of public client a registration is for.
///
/// The distinction is not cosmetic: the two use different redirect URI rules
/// and must not share a `client_id`, because an identity provider's redirect
/// allow-list is per client.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OAuthClientKind {
    /// Desktop or CLI. Redirects to an exact `http://127.0.0.1:<port>/` URI.
    Native,
    /// Single-page app. Redirects to an `https` URL the app serves itself.
    Browser,
}

/// Wire form, accepting either the singular fields or the `clients` list.
///
/// Kept private: callers see an [`OAuthClientConfiguration`] with both forms
/// already reconciled, so nothing downstream has to ask which one arrived.
#[derive(Deserialize)]
struct OAuthClientConfigurationWire {
    issuer: String,
    audience: String,
    required_scope: String,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    flows: Option<Vec<OAuthFlow>>,
    #[serde(default)]
    clients: Vec<OAuthClient>,
}

impl From<OAuthClientConfigurationWire> for OAuthClientConfiguration {
    fn from(wire: OAuthClientConfigurationWire) -> Self {
        let native = wire
            .clients
            .iter()
            .find(|client| client.kind == OAuthClientKind::Native)
            .cloned();

        // Whichever form the provider sent, fill in the other.
        let client_id = wire
            .client_id
            .or_else(|| native.as_ref().map(|client| client.client_id.clone()))
            .unwrap_or_default();
        let redirect_uri = wire
            .redirect_uri
            .or_else(|| native.as_ref().map(|client| client.redirect_uri.clone()))
            .unwrap_or_default();
        let flows = wire
            .flows
            .or_else(|| native.as_ref().map(|client| client.flows.clone()))
            .unwrap_or_default();

        let mut clients = wire.clients;
        if native.is_none() && !client_id.is_empty() {
            clients.insert(
                0,
                OAuthClient {
                    kind: OAuthClientKind::Native,
                    client_id: client_id.clone(),
                    redirect_uri: redirect_uri.clone(),
                    flows: flows.clone(),
                },
            );
        }

        Self {
            issuer: wire.issuer,
            audience: wire.audience,
            required_scope: wire.required_scope,
            client_id,
            redirect_uri,
            flows,
            clients,
        }
    }
}

/// OAuth grants a provider permits a public client to use.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OAuthFlow {
    #[serde(rename = "authorization_code_pkce")]
    AuthorizationCodePkce,
    DeviceAuthorization,
}

/// Identity derived by the provider from a verified bearer token.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthenticatedIdentity {
    pub provider_id: String,
    pub user_id: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// Response containing a project's current commit.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeadResponse<I> {
    pub commit_id: Option<I>,
}

/// Compare-and-swap request for advancing a project head.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdvanceHeadRequest<I> {
    pub from: Option<I>,
    pub to: I,
}

/// Conflict response returned when the expected head is stale.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConflictResponse<I> {
    #[serde(default)]
    pub current: Option<I>,
}

/// Response returned after storing a commit.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PutCommitResponse<I> {
    pub id: I,
}

/// Ordered commit history response.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryResponse<C> {
    pub commits: Vec<C>,
}

/// Destructive history policy applied by a provider.
///
/// There is deliberately no "everything" variant: keeping everything means
/// not calling the retention endpoint. Once a provider has removed a version,
/// changing the desktop setting cannot resurrect it.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "policy")]
pub enum RetentionRule {
    /// Keep the newest `count` versions. Providers always keep HEAD, including
    /// when a malformed client sends `count: 0`.
    Latest { count: u32 },
    /// Keep HEAD and the connected history prefix through the oldest commit at
    /// or after this Unix timestamp.
    Since { timestamp: i64 },
}

/// Retention request plus objects that active client workflows still need.
///
/// Providers must preserve these roots even when they sit outside the visible
/// history boundary. Pending mirror pushes and pre-merge stashes are the two
/// ordinary examples.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetentionRequest<I, H> {
    pub rule: RetentionRule,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protected_commits: Vec<I>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protected_blobs: Vec<H>,
}

impl RetentionRule {
    /// Number of entries to keep from a newest-first linear history.
    pub fn retained_prefix_len(self, timestamps: impl IntoIterator<Item = i64>) -> usize {
        let timestamps: Vec<i64> = timestamps.into_iter().collect();
        if timestamps.is_empty() {
            return 0;
        }
        match self {
            Self::Latest { count } => (count.max(1) as usize).min(timestamps.len()),
            Self::Since { timestamp } => timestamps
                .iter()
                .rposition(|candidate| *candidate >= timestamp)
                .map_or(1, |index| index + 1),
        }
    }
}

/// Result of applying a [`RetentionRule`].
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetentionReport {
    /// Versions that disappeared from this project's visible history.
    pub versions_removed: u64,
    /// Content-addressed objects physically reclaimed during this pass.
    ///
    /// Providers may keep recently orphaned objects for a grace period, so
    /// this can be zero even when `versions_removed` is non-zero.
    pub objects_removed: u64,
    /// Physical encoded bytes reclaimed during this pass.
    pub bytes_freed: u64,
}

/// User-authored metadata attached to a project rather than a specific version.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectMetadata {
    /// Comma-separated musical categories, such as `Drum & Bass, Jungle`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    /// Free-form labels used to organize and find projects.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl ProjectMetadata {
    /// Iterate the normalized, non-empty genres stored in the wire-compatible field.
    pub fn genres(&self) -> impl Iterator<Item = &str> {
        self.genre
            .iter()
            .flat_map(|genres| genres.split(','))
            .map(str::trim)
            .filter(|genre| !genre.is_empty())
    }

    pub fn is_empty(&self) -> bool {
        self.genre.is_none() && self.tags.is_empty()
    }
}

/// Portable placement of a project beneath a user-selected library root.
///
/// `relative_path` always uses `/` separators and never contains the absolute
/// path of the computer that created the backup. For a folder-owned project it
/// names that folder; for a standalone project it names the project file.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectLocation {
    pub relative_path: String,
}

/// Human-facing metadata registered for one provider-scoped project handle.
///
/// The provider cannot infer these values from an opaque handle, and listing
/// projects should not require downloading every project's latest snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectProfile<F> {
    pub display_name: String,
    pub format: F,
    /// Mutable project metadata. Omitted while empty so profiles written before
    /// this field existed retain their compact wire shape.
    #[serde(default, skip_serializing_if = "ProjectMetadata::is_empty")]
    pub metadata: ProjectMetadata,
    /// Where this project belongs beneath a library root. Profiles written by
    /// older clients omit it and continue to restore directly into the folder
    /// selected by the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<ProjectLocation>,
}

/// One project visible to the authenticated provider account.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderProject<I, F> {
    pub handle: String,
    pub head: I,
    /// Absent for projects written by clients predating project catalogues.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<ProjectProfile<F>>,
    /// Timestamp of the HEAD commit, in Unix epoch seconds.
    pub updated_at: i64,
}

/// Response returned by `GET /v1/projects`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectsResponse<I, F> {
    pub projects: Vec<ProviderProject<I, F>>,
}

/// Batch request asking which content hashes already exist.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HasBlobsRequest<H> {
    pub hashes: Vec<H>,
}

/// Parallel response for [`HasBlobsRequest`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HasBlobsResponse {
    pub present: Vec<bool>,
}

/// Error body returned by the HTTP server.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorResponse {
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_legacy_project_without_a_profile_should_still_decode() {
        let response: ProjectsResponse<String, String> = serde_json::from_str(
            r#"{"projects":[{"handle":"song","head":"commit","updated_at":1750000000}]}"#,
        )
        .expect("project list");

        assert_eq!(response.projects.len(), 1);
        assert_eq!(response.projects[0].handle, "song");
        assert!(response.projects[0].profile.is_none());
    }

    #[test]
    fn a_legacy_profile_should_decode_with_empty_metadata() {
        let profile: ProjectProfile<String> =
            serde_json::from_str(r#"{"display_name":"Song","format":"auru"}"#)
                .expect("legacy project profile");

        assert_eq!(profile.metadata, ProjectMetadata::default());
        assert_eq!(profile.location, None);
    }

    #[test]
    fn project_metadata_should_expose_each_comma_separated_genre() {
        let metadata = ProjectMetadata {
            genre: Some("Drum & Bass, Jungle,  Liquid  ".to_owned()),
            tags: Vec::new(),
        };

        assert_eq!(
            metadata.genres().collect::<Vec<_>>(),
            ["Drum & Bass", "Jungle", "Liquid"]
        );
    }

    #[test]
    fn project_metadata_should_round_trip_inside_the_profile_metadata_field() {
        let profile = ProjectProfile {
            display_name: "Night Drive".to_owned(),
            format: "ableton_live_set".to_owned(),
            metadata: ProjectMetadata {
                genre: Some("Drum & Bass".to_owned()),
                tags: vec!["work in progress".to_owned(), "collab".to_owned()],
            },
            location: Some(ProjectLocation {
                relative_path: "Ableton/Projects/Night Drive Project".to_owned(),
            }),
        };

        let encoded = serde_json::to_value(&profile).expect("encode project profile");

        assert_eq!(encoded["metadata"]["genre"], "Drum & Bass");
        assert_eq!(encoded["metadata"]["tags"][1], "collab");
        assert_eq!(
            encoded["location"]["relative_path"],
            "Ableton/Projects/Night Drive Project"
        );
        assert_eq!(
            serde_json::from_value::<ProjectProfile<String>>(encoded)
                .expect("decode project profile"),
            profile
        );
    }

    #[test]
    fn retention_rules_should_keep_a_connected_newest_first_prefix() {
        let timestamps = [300, 200, 100];

        assert_eq!(
            RetentionRule::Latest { count: 2 }.retained_prefix_len(timestamps),
            2
        );
        assert_eq!(
            RetentionRule::Latest { count: 0 }.retained_prefix_len(timestamps),
            1,
            "HEAD can never be removed"
        );
        assert_eq!(
            RetentionRule::Since { timestamp: 150 }.retained_prefix_len(timestamps),
            2
        );
        assert_eq!(
            RetentionRule::Since { timestamp: 999 }.retained_prefix_len(timestamps),
            1,
            "HEAD survives even when every commit predates the cutoff"
        );
    }

    #[test]
    fn oauth_health_metadata_should_round_trip_and_remain_optional() {
        let configured = HealthResponse {
            protocol: WIRE_VERSION.to_owned(),
            provider_id: Some("studio-pm".to_owned()),
            name: Some("Studio PM".to_owned()),
            capabilities: serde_json::json!({}),
            authentication: Some(OAuthClientConfiguration::new(
                "https://auth.example.com",
                "auru-pm",
                "openid",
                vec![OAuthClient {
                    kind: OAuthClientKind::Native,
                    client_id: "auru-desktop".to_owned(),
                    redirect_uri: "http://127.0.0.1:43827/oauth/callback".to_owned(),
                    flows: vec![
                        OAuthFlow::AuthorizationCodePkce,
                        OAuthFlow::DeviceAuthorization,
                    ],
                }],
            )),
        };
        let encoded = serde_json::to_string(&configured).expect("health response");
        let decoded: HealthResponse<serde_json::Value> =
            serde_json::from_str(&encoded).expect("decode health response");
        assert_eq!(decoded, configured);

        let legacy: HealthResponse<serde_json::Value> =
            serde_json::from_str(r#"{"protocol":"auru-pm-v1","capabilities":{}}"#)
                .expect("legacy health response");
        assert!(legacy.authentication.is_none());
    }
}

#[cfg(test)]
mod oauth_client_tests {
    use super::*;

    #[test]
    fn a_provider_publishing_only_the_singular_fields_gains_a_native_entry() {
        // What every server written before `clients` existed sends.
        let json = r#"{
            "issuer": "https://identity.example.com",
            "audience": "auru-pm",
            "client_id": "desktop",
            "required_scope": "openid",
            "redirect_uri": "http://127.0.0.1:43827/oauth/callback",
            "flows": ["authorization_code_pkce"]
        }"#;
        let config: OAuthClientConfiguration = serde_json::from_str(json).unwrap();
        let native = config.client(OAuthClientKind::Native).unwrap();
        assert_eq!(native.client_id, "desktop");
        assert_eq!(native.redirect_uri, "http://127.0.0.1:43827/oauth/callback");
        assert_eq!(native.flows, vec![OAuthFlow::AuthorizationCodePkce]);
        assert!(config.browser_client().is_none());
    }

    #[test]
    fn a_provider_publishing_only_clients_fills_the_singular_fields() {
        // What a third-party provider written against the new shape may send.
        // The desktop client reads the singular fields, so they cannot be empty.
        let json = r#"{
            "issuer": "https://identity.example.com",
            "audience": "auru-pm",
            "required_scope": "openid",
            "clients": [
                {
                    "kind": "browser",
                    "client_id": "dashboard",
                    "redirect_uri": "https://dashboard.example.com/oauth/callback",
                    "flows": ["authorization_code_pkce"]
                },
                {
                    "kind": "native",
                    "client_id": "desktop",
                    "redirect_uri": "http://127.0.0.1:43827/oauth/callback",
                    "flows": ["authorization_code_pkce", "device_authorization"]
                }
            ]
        }"#;
        let config: OAuthClientConfiguration = serde_json::from_str(json).unwrap();
        assert_eq!(config.client_id, "desktop");
        assert_eq!(config.redirect_uri, "http://127.0.0.1:43827/oauth/callback");
        assert_eq!(
            config.flows,
            vec![
                OAuthFlow::AuthorizationCodePkce,
                OAuthFlow::DeviceAuthorization
            ]
        );
        assert_eq!(config.browser_client().unwrap().client_id, "dashboard");
    }

    #[test]
    fn both_forms_survive_a_round_trip() {
        let json = r#"{
            "issuer": "https://identity.example.com",
            "audience": "auru-pm",
            "required_scope": "openid",
            "client_id": "desktop",
            "redirect_uri": "http://127.0.0.1:43827/oauth/callback",
            "flows": ["authorization_code_pkce"],
            "clients": [
                {
                    "kind": "native",
                    "client_id": "desktop",
                    "redirect_uri": "http://127.0.0.1:43827/oauth/callback",
                    "flows": ["authorization_code_pkce"]
                },
                {
                    "kind": "browser",
                    "client_id": "dashboard",
                    "redirect_uri": "https://dashboard.example.com/oauth/callback",
                    "flows": ["authorization_code_pkce"]
                }
            ]
        }"#;
        let config: OAuthClientConfiguration = serde_json::from_str(json).unwrap();
        let round_tripped: OAuthClientConfiguration =
            serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        assert_eq!(config, round_tripped);
        assert_eq!(round_tripped.clients.len(), 2);
    }
}

package studio.auru.pm;

import java.net.URLEncoder;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.Optional;

/** Calls scoped to one project handle. */
public final class ProjectClient {

    private final AuruClient client;
    private final String handle;
    private final String base;

    ProjectClient(AuruClient client, String handle) {
        this.client = client;
        this.handle = handle;
        this.base = "/v1/projects/" + URLEncoder.encode(handle, StandardCharsets.UTF_8);
    }

    public String handle() {
        return handle;
    }

    /**
     * Register this project's human-facing metadata.
     *
     * <p>Also how the handle comes into existence: until a profile is registered every other
     * project-scoped call answers {@code not_found}, blob upload included. Publish one before
     * uploading anything.
     */
    public void putProfile(ProjectProfile profile) {
        client.require(client.capabilities().projectListing(), "project_listing", "project profiles");
        client.request("PUT", base, profile.toJson(), "application/json");
    }

    /** The current HEAD, empty on a project with no commits. */
    public Optional<ContentHash> head() {
        return client.json("GET", base + "/head", null).optString("commit_id").map(ContentHash::parse);
    }

    /**
     * Compare-and-swap HEAD from {@code from} to {@code to}.
     *
     * <p>Pass an empty {@code from} for the initial publish.
     *
     * @throws HeadConflictException when the provider's HEAD is not {@code from}. That exception
     *     carries the actual HEAD, so a caller can rebase without asking again.
     */
    public void advanceHead(Optional<ContentHash> from, ContentHash to) {
        Json.ObjectBuilder body = Json.object();
        body.put("from", from.map(hash -> Json.of(hash.toString())).orElse(Json.NULL));
        body.put("to", to.toString());
        client.json("POST", base + "/head", body.build());
    }

    /**
     * Store a commit.
     *
     * <p>The provider recomputes the id from the canonical encoding and rejects a mismatch — writing
     * a commit you did not compute is an auth-equivalent failure, not a formatting slip. A
     * {@link Commit} always carries a derived id, so this cannot be got wrong by construction.
     *
     * <p>Idempotent: re-posting an existing id succeeds.
     *
     * @return the id the provider stored, always equal to {@code commit.id()}
     */
    public ContentHash putCommit(Commit commit) {
        Json response = client.json("POST", base + "/commits", commit.toJson());
        return ContentHash.parse(response.string("id"));
    }

    public Commit getCommit(ContentHash id) {
        return Commit.fromJson(
                client.json(
                        "GET",
                        base + "/commits/" + URLEncoder.encode(id.toString(), StandardCharsets.UTF_8),
                        null));
    }

    /** History newest first, with the provider's default page size. */
    public List<CommitSummary> history() {
        return history(0, Optional.empty());
    }

    /** History newest first, at most {@code limit} rows. */
    public List<CommitSummary> history(int limit) {
        return history(limit, Optional.empty());
    }

    /**
     * A page of history.
     *
     * @param limit maximum rows, or zero for the provider's default
     * @param before return commits strictly older than this id
     */
    public List<CommitSummary> history(int limit, Optional<ContentHash> before) {
        StringBuilder query = new StringBuilder();
        if (limit > 0) {
            query.append("limit=").append(limit);
        }
        if (before.isPresent()) {
            if (query.length() > 0) {
                query.append('&');
            }
            query.append("before=")
                    .append(URLEncoder.encode(before.get().toString(), StandardCharsets.UTF_8));
        }

        String path = base + "/history" + (query.length() > 0 ? "?" + query : "");
        List<CommitSummary> commits = new ArrayList<>();
        for (Json commit : client.json("GET", path, null).require("commits").elements()) {
            commits.add(CommitSummary.fromJson(commit));
        }
        return commits;
    }

    /**
     * Permanently move the oldest visible-history boundary.
     *
     * <p>Irreversible. Keeping every version means not calling this at all.
     */
    public Retention.Report pruneHistory(Retention.Request request) {
        client.require(
                client.capabilities().historyRetention(), "history_retention", "history retention");
        return Retention.Report.fromJson(client.json("POST", base + "/retention", request.toJson()));
    }

    /** Which of {@code hashes} the provider already holds, parallel-indexed with the request. */
    public List<Boolean> hasBlobs(List<ContentHash> hashes) {
        if (hashes.isEmpty()) {
            return List.of();
        }
        Json body =
                Json.object()
                        .put("hashes", Json.array(hashes.stream().map(h -> Json.of(h.toString())).toList()))
                        .build();
        List<Boolean> present = new ArrayList<>();
        for (Json flag : client.json("POST", base + "/blobs/has", body).require("present").elements()) {
            present.add(flag instanceof Json.Bool value && value.value());
        }
        return present;
    }

    /**
     * Upload a blob under its own hash.
     *
     * <p>The hash is derived from the bytes rather than accepted from the caller, so an upload
     * cannot be filed under the wrong name.
     *
     * <p>Idempotent.
     *
     * @return the hash the blob was stored under
     */
    public ContentHash putBlob(byte[] bytes) {
        ContentHash hash = ContentHash.of(bytes);
        client.requestBytes(
                "PUT",
                base + "/blobs/" + URLEncoder.encode(hash.toString(), StandardCharsets.UTF_8),
                bytes);
        return hash;
    }

    /**
     * Download a blob and check it against its own name.
     *
     * <p>Verification is unconditional: content addressing is only worth anything if the reader
     * checks, and a caller who has to remember to do it separately eventually will not.
     *
     * @throws AuruException if the provider returned bytes that do not hash to {@code hash}
     */
    public byte[] getBlob(ContentHash hash) {
        byte[] bytes = getBlobUnverified(hash);
        if (!hash.matches(bytes)) {
            throw new AuruException(
                    ErrorCode.BAD_REQUEST,
                    "blob " + hash + " does not hash to its own name; the provider returned different bytes");
        }
        return bytes;
    }

    /**
     * Download a blob without checking it.
     *
     * <p>The escape hatch for a caller verifying elsewhere — hashing incrementally while streaming
     * a large sample, say. Named so that skipping the check is a deliberate choice at the call
     * site rather than something a reader has to notice is missing.
     */
    public byte[] getBlobUnverified(ContentHash hash) {
        return client.requestBytes(
                        "GET",
                        base + "/blobs/" + URLEncoder.encode(hash.toString(), StandardCharsets.UTF_8),
                        null)
                .body();
    }

    /**
     * The {@link ProjectInfo} a commit points at.
     *
     * <p>The call a dashboard should reach for: a few kilobytes rather than the snapshot's several
     * megabytes. Empty when the commit predates summaries or its format could not be summarized, in
     * which case a caller falls back to reading the snapshot.
     */
    public Optional<ProjectInfo> projectInfo(Commit commit) {
        return commit.metadata().map(hash -> ProjectInfo.parse(getBlob(hash)));
    }
}

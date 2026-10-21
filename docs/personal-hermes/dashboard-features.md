# Manager dashboard: data management and editors

This is the next dashboard feature specification. It describes requested
behavior, not completed functionality. The [current scaffold](dashboard.md)
has an overview and the first page of read-only assignment, terminal-runtime and
audit lists. It has no template editor, assignment editor or administrator editor.

Use the [technology and implementation conventions](dashboard-stack.md) for
the selected frontend libraries, schema/editor integration, API/state boundaries,
accessibility and testing. The feature behaviors here remain independent of
which portions of that stack have already been installed.

## Data views and navigation

Provide Users, Hermes templates, Terminal backend templates, Assignments,
Administrators and Activity views. Expose manager-domain records through typed,
versioned Rust APIs. The browser has no PostgreSQL connection, SQL console,
arbitrary table editor or direct Kubernetes/Keycloak credentials.

Every collection needs server-side search, filters, sorting and bounded
pagination. Put search/filter/sort state in the URL so a view can be refreshed or
shared. Define each endpoint's supported fields, matching rules and cursor order;
changing filters or sort order resets pagination. Display loading, empty,
permission-denied, unavailable and stale-data states separately. A failed query
must not appear as an empty database. Long IDs/configuration and tables must
remain usable on narrow screens and through keyboard navigation.

| View | Columns and filters | Inspection and actions |
| --- | --- | --- |
| Users | Canonical issuer/subject, optional display name/email, directory-verification status, assigned Hermes template/version, access and instance state; filter by assignment, template, access/state and source | Show identity provenance, current assignment and related instance; create/change an assignment through an authorized workflow |
| Hermes templates | Stable template ID, version, image digest, configuration summary, source/ownership, draft/published state and assignment count | Inspect deployment inputs and managed Hermes config, view versions/diffs and affected users, create/clone/edit a draft, validate and publish |
| Terminal backend templates | Stable runtime ID, immutable version, image, SSH contract, resources and permitted storage/mount summaries | Inspect/edit the typed runtime definition and owner-specific binding choices; show assignments allowed to request it |
| Assignments | Exact user identity, Hermes template/version, source/ownership, access, desired/observed generation and condition | Inspect desired/applied configuration, assign/change a Hermes template, inspect execution policy and operation history |
| Administrators | Canonical issuer/subject, optional identity labels, effective access, grant source and audit metadata where available | Inspect effective administrator permission and add/revoke access through the chosen authority described below |
| Activity | Actor, action, affected user/template/assignment, operation, outcome and time | Open related records and redacted change/operation details |

The initial Users collection consists of manager-registered identities; it must
not pretend to contain every Keycloak user. Optional Keycloak directory search
can supply additional identity candidates and descriptive fields. Show whether
an identity is directory-verified or manually entered. Exact `(issuer, subject)`
is authoritative; email/display name are labels and search aids, never ownership
or authorization keys. A user need not have a Hermes assignment to be an admin,
and a Hermes assignment does not grant administrator access.

## Hermes template inspection and editing

A Hermes template includes deployment inputs and its managed Hermes
configuration. The existing typed definition already contains:

```json
{
  "schemaVersion": 1,
  "image": "registry.example.org/platform/hermes@sha256:<64 lowercase hex digits>",
  "storageClass": "local-path",
  "homeGib": 5,
  "workspaceGib": 10,
  "cpuMillis": 1000,
  "memoryMib": 2048,
  "adminConfig": {
    "agent": { "max_turns": 30 }
  }
}
```

The image placeholder above is illustrative and intentionally not a valid image
reference. `adminConfig` belongs to the template, so selecting that template
preconfigures administrator-enforced Hermes settings for the assigned user. It
is not merely an initial user-config seed. Other native user settings remain on
the user's home PVC and editable through Hermes.

The template detail view has Summary, Deployment, Managed Hermes configuration,
Versions and Assigned users sections. Show the source/owner, schema/runtime
compatibility, content digest, publication/validation state and any controller
rejection. Assigned users links back to a filtered assignments table. Show the
desired template/version separately from the version observed on an instance.

Provide two synchronized editors over one draft:

- **Form:** image digest, CPU/memory with units, storage class and home/workspace
  capacities. A dedicated Managed Hermes configuration section edits
  `adminConfig`, with grouped fields, descriptions, defaults and controls for
  supported agent/plugin/terminal settings.
- **Raw configuration:** the complete typed template as JSON, with YAML as an
  optional authoring format that converts to the same JSON data model. Expose
  `adminConfig` as an object within the template, including an optional focused
  editor for that object. This is not an arbitrary Kubernetes manifest editor.

Switching modes must preserve valid configuration, including supported fields
that the form does not yet understand. Do not silently drop advanced fields.
When raw input is syntactically invalid, retain it and explain why the form view
cannot be updated. Show dirty state, unsaved-change navigation protection and
errors at both form field paths and raw-editor line/column positions.

Configuration assistance must include:

- Completion, documentation and type hints from a versioned template schema and
  the supported pinned Hermes configuration schema. Match assistance to the
  selected image/runtime version; do not assume an arbitrary image tag describes
  the installed Hermes schema.
- References and searchable selections for registered templates, terminal runtime
  IDs/versions and approved storage classes/bindings. Static schemas can supply
  enum values; dynamic suggestions come from authorized, bounded manager APIs.
- Syntax/type checks while editing, followed by authoritative server validation.
  Rust validates the typed envelope, policy and references. Hermes-setting
  validation must use reviewed schema support for the selected runtime; keys
  without such support are marked unvalidated rather than claiming complete
  Hermes type checking. Arbitrary image/plugin code is not executed to validate
  a submitted configuration.
- Structured validation results containing paths, severity, codes and actionable
  messages, without credential values or unrelated user data. Validate again on
  publish and assignment; browser validation alone never authorizes a write.

The Managed Hermes configuration preview must distinguish three layers:

1. Template-selected `adminConfig` overrides, such as an agent turn limit or a
   pinned default `terminal.runtime_id`.
2. Infrastructure-required settings: `terminal.backend=managed_execution`, the
   required image-owned plugin and protected connection/credential paths. These
   cannot be disabled by either editor. Show them as enforced and explain their
   origin; no credential value is displayed.
3. Settings left to the user. Omitting a managed override leaves that setting
   editable in Hermes, subject to mandatory platform policy. Removing an override
   can expose the user's retained value again; it does not erase their config.

Preview only the managed policy and a generic/default merge. Do not read a
user's private native configuration to render an administrator preview.
`adminConfig` must not contain model keys, passwords, personal prompts or other
private seed data. Use approved credential references and private delivery paths.
The manager continues to enforce allowed terminal IDs independently of a
template's preferred ID; choosing a default does not grant backend permission.

## Template versions, ownership and rollout

An editor operates on a draft. Saving a draft does not modify a live template or
restart an instance. Publishing validates and creates an immutable version;
existing version history and in-flight operation snapshots remain intact.
Provide a semantic diff, changed managed-setting paths and an affected-user
preview before applying a new version. Apply/rollout is an explicit action with
visible scope and an operation result. Do not mutate an immutable catalog row.

The UI must clearly identify pinning versus following template revisions and
which users are affected. The precise follow-latest/rollout policy is a design
decision for the authoring API; the UI must not silently assume every draft or
newly published version applies to every user. Describe Recreate restart and
drain effects before applying changes. Storage-class changes and volume shrinking
remain explicit migration requirements, not automatic PVC replacement.

Current commissioning has separate infrastructure-owned desired template/user
rows and controller-owned immutable snapshots and assignments. Writing an active
infrastructure definition currently advances mapped users' desired generations;
it is not a draft API. Dashboard drafts must not be stored as edits to those live
rows. The controller does not silently adopt assignments from another management
source. Preserve this boundary when adding template and assignment write APIs:

- Show infrastructure-owned rows and snapshots with their source and effective
  values. Keep them read-only in the dashboard unless an explicitly designed,
  audited ownership-transfer or source-edit workflow authorizes the change.
- Define manager-owned authoring records and typed application commands for
  dashboard-managed templates/mappings, wired to native commissioning through
  the same generation/fencing/journal rules. Do not bypass lifecycle logic by
  letting the browser or a generic backend editor write controller history.
- Surface conflicting infrastructure updates and optimistic concurrency failures
  for review. Do not repeatedly overwrite the other source's desired state.

This authoring/ownership integration is required backend work. Current approved
template summaries are not a complete editable native-template API.

## Assigning templates to users

From either User details or Template → Assigned users, allow an administrator to
select an exact identity and an approved Hermes template/version. Show its managed
`adminConfig` summary and enforced platform settings before applying it. For an
existing assignment, show current and proposed templates/configuration, affected
storage/resources and restart implications. A user retains their instance
identity, PVC data and credentials; selecting another template cannot transfer
ownership to a different subject.

Allow a separately reviewed bulk operation for explicitly selected users, with
per-user eligibility, validation and outcomes. Do not apply to hidden rows merely
because a filter matched them. Define bounded batch limits, durable per-user
results and idempotent retry behavior before exposing bulk apply.

All mutations require server authorization, browser CSRF, version preconditions,
idempotency and actor-attributed audit. Return operation IDs and pending/error
status; a successful HTTP acknowledgement is not a claim that rollout completed.
The native template choice and terminal-backend allowlist remain independent.
Missing identity, unknown version, rejected config, ownership conflict and storage
migration requirements must prevent dispatch and produce specific UI feedback.

## Administrator list and authorization authority

**Current implementation:** Keycloak authenticates the human and the manager
requires the configured management-client role in its signed access token.
For the example configuration, this is
`resource_access["manager-admin"].roles` containing `platform-administrator`, plus
the trusted issuer, `manager-api` audience and authorized client. Manager
PostgreSQL stores browser sessions, audit and assignments; it has no application
administrator-grant table. Adding a desired user/assignment does not grant admin
access. A generic realm `admin` role is insufficient.

**Requested addition:** administrators must be able to inspect and edit who has
manager administration access. This requires an explicit authorization-authority
decision before implementing the write path. Options are:

| Authority | Meaning of an edit | Required backend change |
| --- | --- | --- |
| Keep Keycloak client roles authoritative | Add/revoke the dedicated manager client-role mapping in Keycloak; the dashboard displays effective role membership, including inherited membership | A separately reviewed, narrowly authorized Keycloak role-management adapter. The current directory boundary is read-only and cannot make these writes. The manager must not receive unrestricted realm administration merely to edit this role. |
| Manager PostgreSQL grants | Add/revoke an installation-scoped grant for an exact verified `(issuer, subject)`; Keycloak continues to authenticate the user | Grant schema, bootstrap/recovery and management commands, plus changes to every browser and bearer admin authorization path. Decide whether the Keycloak client role remains an additional prerequisite or is replaced by the manager grant; no implicit OR fallback. |

Recommended direction for review: manager-owned PostgreSQL grants fit an editable
application administrator list while preserving read-only Keycloak directory
access. Keycloak would continue to authenticate humans and supply exact stable
identities; the manager would own its application permission list. This would
avoid giving the manager Keycloak role-write privileges. It is a proposal, not an
accepted change to the current role gate. Explicitly resolve the role prerequisite
and initial-admin bootstrap before implementation.

Do not implement two independently writable grant authorities with ambiguous
results. The administrator list must display the effective source of permission;
do not infer the full admin population from recent session or audit rows.
Inherited Keycloak grants must not appear revoked when a direct mapping is
removed but another grant still applies.

For either design, use canonical identity selection, audit grant/revoke actor
and target, require explicit permission to manage administrators, prevent
accidental removal of the last recoverable administrator and document bootstrap
and recovery. Specify revocation propagation for active sessions and API tokens;
it must apply to pages, assets, API reads and writes. The dashboard must explain
any remaining token/session refresh delay. Do not change the live gate as part
of documenting this requirement.

## Delivery and acceptance

1. Extend read APIs with user/template details, source/ownership, search/filter/
   sort and pagination; add inspectable tables and template-to-user links.
2. Add versioned schemas, server validation and template drafts; implement the
   synchronized form/raw `adminConfig` and deployment editors.
3. Complete native-template authoring/ownership integration and explicit
   publication/application, then single-user assignment and reviewed bulk apply.
4. Select and implement one administrator authority, including its bootstrap,
   revocation and last-admin recovery behavior, then build the editable list.

Acceptance must prove form/raw round trips without lost fields; path-addressed
syntax/type/policy errors; rejection of disabled managed execution; template
`adminConfig` enforcement after assignment and restart; unaffected retained user
settings; immutable history; correct affected-user previews; exact identity
selection; search/filter pagination consistency; optimistic conflicts and replay;
source-ownership protection; administrator grant/revoke and active-session behavior;
and rejection of non-admin reads/writes. Test the read and write workflows through
the API-only boundary as well as the browser.

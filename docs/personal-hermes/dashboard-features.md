# Manager dashboard: data management and editors

This specification defines the dashboard behavior. The implementation now includes
searchable views, typed administration commands, template form/JSON authoring,
immutable publication, explicit version application and PostgreSQL administrator
management. The [operator guide](dashboard.md) explains setup and authority;
the [work plan](dashboard-workplan.md#status) records acceptance evidence and
implementation scope. YAML authoring and optional read-only Keycloak directory lookup are implemented.

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

The recorded Users collection consists of manager-registered identities and verified
manager logins, with their provenance. The separate optional Keycloak directory
search supplies additional identity candidates and descriptive fields. Show whether
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

**Accepted authority:** Keycloak authenticates the human and supplies the exact
stable issuer/subject. Installation-scoped PostgreSQL administrator grants are
the sole authorization authority. Keycloak client and realm roles do not grant
access and are not an additional prerequisite. User assignments do not grant
administration permission.

The dashboard lists actual grants and their PostgreSQL source. A separate
`canManageAdministrators` permission controls grant writes. Targets must have
verified authentication evidence; labels and email are never identity keys.
Changes use ETags, actor-scoped idempotency and audit. Transactions serialize
grant changes and refuse removal or demotion of the last recoverable manager.
The operator has explicit initial-bootstrap and recovery commands using a
private identity file, documented in the [operator guide](dashboard.md#keycloak-identity-and-postgresql-administrator-access).

Current grants are checked on every browser and bearer request, including pages
and assets, and again in mutation transactions. Revocation fences browser
sessions and denies the next request without waiting for token refresh. No
Keycloak write adapter, parallel grant authority or implicit role fallback exists.

## Delivery and acceptance

The [work plan](dashboard-workplan.md) breaks this into packages and records
the original API gaps and their implementation evidence. Search/filter/keyset
pagination, definition reads, source metadata, explicit template application,
pointer-addressed validation and entity-aware audit are now implemented. Several existing endpoints
(assignment creation and access, retry, operations, execution policy, runtime
and storage registration) already support UI workflows without backend work.

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

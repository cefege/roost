# Unified access and onboarding

Status: proposed; research only. No service, network, authentication or production changes applied.

## Recommendation

Borrow Pi Pocket's access launcher and sign-in interaction, not its agent backend. Keep Roost's Rust/Dioxus client, terminal-only product, device keys and scoped pairing grants. Present one Roost installation with several ways to reach it, rather than making users assemble coordinator addresses, worker enrollment and browser pairing themselves.

The remembered reference is [TannerMidd/pi-pocket](https://github.com/TannerMidd/pi-pocket). An earlier Roost conversation explicitly requested its UI. Its launcher selects local, LAN, Cloudflare or Tailscale access, checks prerequisites, remembers the choice, and presents an open action, sign-in link and QR code. Its [sign-in screen](https://github.com/TannerMidd/pi-pocket/blob/main/web/signin.js) accepts a link, token or invite code in one field.

Its [folder picker](https://github.com/TannerMidd/pi-pocket/blob/main/web/sheets/folder.js) uses places, breadcrumbs, a current location, and an explicit Open action; mouse selection and touch navigation differ intentionally. These are interaction references, not dependencies to restore.

## Current evidence

- `crates/roost-cli/src/quickstart/` already installs a local coordinator and worker, promotes a declared HTTPS endpoint, joins workers, and issues browser/machine enrollment grants.
- `crates/roost-web/src/components/settings_navigation.rs` splits Network into Machines, Connection and Paired devices. Phone pairing and machine enrollment separately assess external addresses.
- `crates/roost-web/src/components/access_gate.rs` presents Checking access without endpoint selection or recovery. Connection settings are inside the protected shell.
- `crates/roost-web/src/components/settings/connection.rs:69-151` labels the stored URL Connected. `crates/roost-client-core/src/client/local/bootstrap.rs:173-194` instead gives worker bootstrap precedence and ignores a stored override without the self-hosted marker. A selected preference is therefore not proof of the effective endpoint or connectivity. This is source evidence, not a reproduced browser failure.
- `crates/roost-host/src/coord_config_loader.rs` owns coordinator configuration validation. Reuse it; do not add a second validator in the wizard.
- Roost currently documents manual Tailscale and Cloudflare setup. Owning their provisioning and lifecycle would be new functionality, not merely a new settings page.

## User experience

### First run

Offer two intentions:

1. **Set up this Roost**: install the coordinator and worker together on the first machine through the existing quickstart path.
2. **Connect to an existing Roost**: open its address or pairing link. Browser pairing and adding this computer as a worker remain separate actions with separate credentials.

Then present **This computer**, **Private access — Tailscale**, and **Public HTTPS — Cloudflare/custom proxy**. These choose reachability, not three different account/login providers. Recommend Tailscale Serve HTTPS over plain tailnet HTTP; do not expose an unauthenticated LAN listener as a shortcut.

A single Access screen serves onboarding, Settings and recovery. It shows:

- The effective Roost server address and why it was selected.
- Reachability and browser authorization as separate states.
- Configured local/private/public addresses, with Open, Copy and Pair another device actions where applicable.
- Add machine separately from Pair browser/phone.
- Configuration ownership: local service or externally managed deployment.
- Specific failures: DNS/TLS/proxy, edge sign-in, unpaired/revoked browser, unavailable coordinator, or offline worker.

Offer one input for a Roost address, pairing link or existing supported pairing credential, plus QR scanning/opening. Parse the existing credential formats; do not introduce a permanent owner token. Keep device approval and revocation intact.

### Folders

Keep machine identity explicit when browsing a filesystem. Adapt Pi Pocket's places/breadcrumb/Open interaction to Roost's existing folder picker and touch behavior. Retain existing workspace IDs and user-controlled synchronized folder order. Changing the route to the same installation must not create another folder inventory or another session history.

## Coordinator decision

Keep the coordinator. It owns authorization, revocation, the worker registry, shared folder/session state, Sync and direct-route negotiation. Removing it requires a replacement control-plane architecture, not a login redesign.

For a fresh single-machine installation, it is a colocated background role installed with the worker, not another server the user must provision. Additional machines join that installation with workers only.

For our fleet, adopt the existing desktop-pc k3s coordinator and Cloudflare/Caddy edge. Do not start a host coordinator or run generic host quickstart to repair it. Flux remains the deployment owner.

An outage screen must remain accessible before authorization completes. Already-established qualified direct terminals may continue only while their grants remain valid. New grants, renewal, negotiation and fleet controls still require the coordinator. Recovery must not reset device keys, reload working direct terminals automatically, or manufacture offline authority.

## Security and origin constraints

- Browser Ed25519 keys remain non-extractable and origin-scoped. Opening localhost, a Tailscale hostname and a Cloudflare hostname creates separate browser storage contexts. Pair a new origin explicitly using existing one-shot grants; do not promise shared login by exporting keys.
- Keep a canonical browser origin. Additional access addresses require explicit registration and coverage by existing CSP/CORS/Sync admission rules. No automatic cross-origin failover or arbitrary coordinator retargeting.
- Preserve worker-served bootstrap precedence and the existing self-hosted override trust boundary. Show the actual endpoint, not a duplicated UI interpretation of storage.
- Cloudflare Access identity is provenance/an additional gate, not a Roost principal. Tailnet membership likewise does not enroll a browser or worker.
- Remote enrollment requires a validated reachable HTTPS declaration. An invalid explicit declaration is an error, never permission to guess another address or mint an unusable grant.
- Preserve forwarded-header locality protections, private-path filtering and the device-auth refusal marker. A proxy login failure is not device revocation.
- Keep credentials out of query strings, persistent UI state and logs. Do not copy Pi Pocket's long-lived owner sign-in link into Roost.

## Implementation sequence

### 1. Consolidate endpoint and access truth

Add one client-core access presentation model, fed by the existing bootstrap, transport and authorization owners. Include document origin, effective coordinator endpoint, selection source, declared external address, local worker-door state, reachability and authorization separately. Public recovery exposes only safe connection information, never protected fleet state.

Move duplicated browser/phone/machine address assessments behind the existing enrollment decision logic. Fix the stored-selection/actual-endpoint disagreement at the bootstrap owner. Preserve the distinction between coordinator port 4113 and worker door 4114.

If new server capabilities/configuration facts need an RPC, define the protobuf contract first, implement it in the existing coordinator service, and project it through client-core. Do not build a second RPC stack or infer deployment ownership from a failed host-service probe.

### 2. Build one access surface

Reuse the current pairing requester, approver, approval dialog, QR redemption and device inventory. Make connection diagnosis, retry and address opening reachable outside the protected shell without weakening its closed authorization default.

Replace the scattered connection guides with this surface; link machines and paired devices into it without merging their credential types. Use existing rust-ui/Dioxus components and hooks on desktop and mobile. Do not port Preact, Pi Pocket CSS, provider settings or agent features.

### 3. Own network setup safely

Create a host-owned access controller shared by a `roost access` CLI wizard and authorized settings operations. Local installation/profile changes reuse quickstart and the host configuration loader. Preview changes before applying; privileged changes require the relevant operator/service permissions. Operations are typed, bounded and allowlisted, never arbitrary browser-supplied shell commands.

- **Tailscale:** verify installation, login/approval, HTTPS name, certificates and Serve conflicts. Track exact Roost-owned mappings durably; reuse compatible pre-existing mappings without claiming their ownership. Remove only Roost-owned mappings when disabled. Never enable Funnel implicitly or fall back silently to local-only access.
- **Cloudflare:** use a stable named tunnel/custom domain for persistent installations. Retain the filtering proxy and correct forwarded-header handling. Store credentials in the deployment's secret mechanism, not browser storage. A random quick-tunnel URL is unsuitable as the fleet's canonical address because restarts change origins, pairing context and worker endpoints.
- **Externally managed deployments:** detect and display the configured owner. Adopt the existing route and generate reviewable configuration changes for its deployment source. Do not mutate live Flux resources or start competing host services. The UI must distinguish verified current configuration from a proposed unapplied change.

Validate candidate configuration before publication, preserve the previous working route until the replacement is verified, and recover interrupted application without losing configuration ownership. Use the configured worker origin when it differs from the browser origin. Do not disrupt keeper-owned PTYs or overwrite unrelated proxy/VPN configuration.

### 4. Adapt the folder picker

Reuse existing filesystem browsing and folder/workspace state. Add the places/breadcrumb/Open interaction through current UI primitives, with deliberate touch and keyboard behavior. Keep this independent of endpoint configuration: changing access mode must not mutate machines, folder identity or saved order.

### 5. Verify and cut over

Reuse existing tests; add regression coverage only for uncovered consumer-visible paths. Verify each implementation phase before committing it, including actual browser/CLI surfaces and the required design review for web UI changes. Update the operating docs and remove superseded guidance in the same change.

Required scenarios:

- Fresh local install, existing-installation join, browser approval and phone QR redemption.
- Worker-served bootstrap, saved override with/without its self-hosted marker, and UI agreement with the actual endpoint.
- First load with no coordinator; recovery remains usable and protected data remains inaccessible.
- Cloudflare edge login without device pairing; proxy redirect/unmarked 401 without false revocation.
- Tailscale unavailable, awaiting approval, port conflict, pre-existing mapping, interrupted setup, and no accidental public Funnel.
- Localhost-to-HTTPS origin transition with explicit pairing and the original device identity preserved at its original origin.
- Coordinator interruption while a qualified direct terminal remains; grant expiry then correctly ends unavailable authority.
- Stable folder/session identity and keeper PID across endpoint promotion; no secret leakage in logs or copied ordinary URLs.
- Production ownership recognized as k3s/Flux, with no duplicate coordinator or unauthorized live configuration mutation.

## Upstream sources

- [Access modes and launcher behavior](https://github.com/TannerMidd/pi-pocket#remote-access)
- [Access selection implementation](https://github.com/TannerMidd/pi-pocket/blob/main/src/launcher/access.ts)
- [Tailscale preflight, conflict detection and ownership](https://github.com/TannerMidd/pi-pocket/blob/main/src/launcher/serve.ts)
- [Service lifecycle and sign-in behavior](https://github.com/TannerMidd/pi-pocket/blob/main/docs/systemd.md)

## Implementation and verification

The five implementation steps are complete. The folder picker retains its
existing scoped navigation owner; access surfaces reuse the stock UI recipes.

- 105 focused native tests passed across host, client-core, coordinator, CLI
  and web access contracts. Scoped clippy and the repository linter passed;
  the linter reported 7,554 inputs and zero violations. The Dioxus web build
  completed successfully.
- An isolated real coordinator and enrolled worker exercised manual pairing,
  worker-served endpoint discovery, desktop folder selection/drill/keyboard
  navigation, and the phone touch-handler path. WebKit rendered the recovery
  surface at desktop and 390×844 phone dimensions; phone touch events were
  dispatched synthetically, not from physical hardware.
- A real WebRTC terminal accepted input while that coordinator was stopped.
  Opening and scrolling recovery kept the same terminal DOM. Coordinator
  restarts and browser-origin changes retained the keeper PID and workspace.
- A decoded phone QR exposed the old worker-origin selection bug. After the
  fix, the actual rendered QR selected the distinct browser front door and
  redeemed successfully at a fresh private HTTPS origin; approval survived
  reload. Another device's replay was rejected. Redeemed grants were absent
  from ordinary server bookmarks.
- Configured Flux ownership remained explicitly unverified. Its browser
  preview produced a source/values proposal, retained old browser and worker
  routes, and offered no Apply action. Retained browser Host admission
  returned 200; an unrelated CORS-only Host returned 403.
- The native local wizard dry run installed no definition, and a foreground
  coordinator refused Tailscale management. Helm lint/template and the
  generated Caddy filter passed isolated runtime checks, including private
  path denial and forwarding-header replacement.

Verification limits: no live host-service installation, Tailscale Serve edit,
Cloudflare policy change or production Flux mutation was performed. The HTTPS
fixture used a private test CA accepted only by managed Chromium; it installed
no system trust. Native deployment/recovery behavior was exercised by the
focused regression tests, not by replacing this workstation's services.
Service-log redaction was not independently verified.

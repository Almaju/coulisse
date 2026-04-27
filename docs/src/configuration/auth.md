# Authentication

Coulisse exposes two HTTP surfaces and lets you guard each one independently.

- `auth.proxy` gates the OpenAI-compatible `/v1/*` endpoints that SDK clients call.
- `auth.admin` gates the `/admin/*` surface — the [Studio UI](../features/studio-ui.md) and every admin endpoint that backs it.

Both scopes are optional. Omit a scope's block and that surface is served unauthenticated — fine for a loopback-only dev box, never for anything reachable beyond it.

Each scope accepts the same shape: exactly one of `basic` or `oidc`. They are mutually exclusive within a scope (the validator rejects both-or-neither). The two scopes are wired separately, so you can run Basic on `/v1/*` for SDK clients while pinning the studio behind OIDC, or vice versa.

## Shape

```yaml
auth:
  proxy:                     # optional; gates /v1/*
    basic:                   # — or — oidc: { ... }
      password: <secret>
      username: <name>       # optional, defaults to "admin"
  admin:                     # optional; gates /admin/*
    oidc:                    # — or — basic: { ... }
      issuer_url:    https://authentik.example.com/application/o/coulisse/
      client_id:     coulisse-admin
      client_secret: <secret>
      redirect_url:  http://localhost:8421/admin/
      scopes:        [email, profile]
```

## `auth.<scope>.basic`

Static HTTP Basic credentials. Best for local dev or a single-operator deployment.

| Field      | Type   | Required | Default | Notes |
|------------|--------|----------|---------|-------|
| `password` | string | yes      | —       | Non-empty. There is no token revocation — rotate if it leaks. |
| `username` | string | no       | `admin` | Non-empty when set. |

Every request to the gated surface must carry `Authorization: Basic <base64(user:pass)>`. Browsers prompt via the native login dialog and cache credentials per origin. The check is constant-time.

## `auth.<scope>.oidc`

Authorization-code-with-PKCE login against any OIDC-compliant IdP — Authentik, Keycloak, Auth0, Google, Microsoft, Okta. Coulisse does not maintain its own user database; *who* may log in is delegated to the IdP's application policy, and any successfully authenticated user is treated as authorized for that scope.

| Field           | Type           | Required | Default            | Notes |
|-----------------|----------------|----------|--------------------|-------|
| `client_id`     | string         | yes      | —                  | Must match the client registered at the IdP. |
| `client_secret` | string         | no       | —                  | Required for confidential clients (Authentik's default); omit for public clients using PKCE only. |
| `issuer_url`    | string         | yes      | —                  | IdP issuer. For Authentik: `https://<host>/application/o/<app-slug>/`. |
| `redirect_url`  | string         | yes      | —                  | Public base URL inside the protected scope. Must be registered as the redirect URI at the IdP. Every subpath of this URL is treated as a valid redirect. |
| `scopes`        | `list<string>` | no       | `[email, profile]` | Extra OAuth2 scopes. `openid` is added automatically. |

On first request, the user is redirected to the IdP to authenticate; afterwards an encrypted session cookie keeps them signed in until 8 hours of inactivity elapse. **Sessions live in process memory** — they evaporate on restart, and users re-authenticate silently if their IdP session is still valid.

The OIDC issuer is contacted at startup via `/.well-known/openid-configuration`. Discovery failure is fatal and surfaces in the startup log.

## Common shapes

### Local dev, single operator

Basic on the studio, leave the proxy open behind your loopback:

```yaml
auth:
  admin:
    basic:
      password: choose-something-strong
```

### SDK clients in one network, studio behind SSO

Basic on the proxy (a single shared bearer-style credential for your services), OIDC on the studio:

```yaml
auth:
  proxy:
    basic:
      username: services
      password: ${COULISSE_PROXY_PASSWORD}
  admin:
    oidc:
      issuer_url:    https://authentik.example.com/application/o/coulisse/
      client_id:     coulisse-admin
      client_secret: ${COULISSE_OIDC_SECRET}
      redirect_url:  https://coulisse.example.com/admin/
```

YAML doesn't expand `${...}` itself; substitute at deploy time (helm, envsubst, sops, etc.).

### Same OIDC IdP for both scopes

The two blocks are independent, so repeat the OIDC config under both — typically with different `client_id`s so the IdP can distinguish humans from machines:

```yaml
auth:
  proxy:
    oidc:
      issuer_url:    https://authentik.example.com/application/o/coulisse/
      client_id:     coulisse-proxy
      client_secret: ${COULISSE_PROXY_OIDC_SECRET}
      redirect_url:  https://coulisse.example.com/v1/
  admin:
    oidc:
      issuer_url:    https://authentik.example.com/application/o/coulisse/
      client_id:     coulisse-admin
      client_secret: ${COULISSE_ADMIN_OIDC_SECRET}
      redirect_url:  https://coulisse.example.com/admin/
```

## Authentik setup

Create an OAuth2/OpenID Provider and an Application bound to it. Set the redirect URI to the `redirect_url` from the YAML — Authentik allows every subpath of a registered redirect by default. Point Coulisse at the provider's issuer URL (the slug-based form shown above). Restrict access via Authentik's policy bindings on the Application; Coulisse honours whatever the IdP returns.

## Leaving a scope open

Omitting a scope's block is the explicit "no auth" mode. That's the right call when:

- The surface is on a loopback-only socket, or
- You terminate auth at your infrastructure layer (oauth2-proxy, Cloudflare Access, Caddy `forward_auth`, a VPN, an SSH tunnel).

Do **not** expose an unauthenticated `/admin/*` to the network — the admin surface can edit live config, including provider keys.

## Restart vs hot-reload

Auth is one of the sections that **does not** hot-reload. Editing `auth:` in `coulisse.yaml` while the server is running has no effect until the next restart. Validation still runs on the file watcher, so a malformed `auth` block is logged but not applied.

## Validation

On startup, `coulisse.yaml` is rejected if:

- A present scope (`auth.proxy` or `auth.admin`) declares neither `basic` nor `oidc`, or both.
- `auth.<scope>.basic.password` or `auth.<scope>.basic.username` is empty.
- `auth.<scope>.oidc.client_id`, `issuer_url`, or `redirect_url` is empty.
- The OIDC issuer cannot be discovered at startup.

The startup banner prints one line per scope (`unauthenticated`, `basic auth enabled`, `OIDC login enabled`) so you can confirm the running posture at a glance.

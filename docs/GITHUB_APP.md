# Connecting Vogt to GitHub with a GitHub App

Vogt's forge adapter reads GitHub (Actions runs, releases, issues, pull
requests, repository posture) and, with write-back enabled, writes to it. It
can authenticate in two ways:

| | Personal access token | GitHub App |
|---|---|---|
| Config | `github_token_file` | `github_app_id`, `github_app_private_key_file`, optionally `github_app_installation_id` |
| Rate limit | 5,000/h, **shared** with everything else on that user | 5,000/h or more **per installation**, Vogt's alone |
| Acts as | the user | the App (`your-app[bot]`) |
| Notifications collector | yes | no — uses `github_token_file` if that is also set |

Both are fully supported. A personal token is the quickest start. Use an App
when the sweep competes for rate limit with other tools on the same account,
when you want permissions narrower than a token's scopes, or when you want
writes attributed to a bot.

When the App settings are set, they take precedence over `github_token_file`
for every GitHub read and write except notifications. If the key file is
missing or unreadable, Vogt logs a warning once and falls back to the token
file.

## 1. Create the App

On GitHub, go to **Settings → Developer settings → GitHub Apps → New GitHub
App**: under your user, or under an organisation's settings to make the
organisation own it.

- **Homepage URL:** anything. Your Vogt URL is fine.
- **Webhook:** untick **Active**. Vogt polls, so it needs no webhook.
- **Where can this GitHub App be installed?** Choose **Only on this account**,
  unless you will install it on accounts you don't own.

Grant only the permissions for the features you use:

| Permission | Access | Used for |
|---|---|---|
| Repository → Metadata | Read | Mandatory; repository identity |
| Repository → Actions | Read | `gh-actions` runs and jobs |
| Repository → Contents | Read | Releases, compare, the posture tree probe, clone on import |
| Repository → Administration | Read | Posture: vulnerability alerts and automated security fixes |
| Repository → Issues | Read | Issue sync. **Read and write** for write-back |
| Repository → Pull requests | Read | Pull-request sync. **Read and write** for write-back |
| Repository → Contents | Read and write | Only for `forge.publish`, which pushes the first branch |
| Repository → Administration | Read and write | Only for `forge.publish`, which creates the repository |
| Organization → Members | Read | Classifying authors as organisation members |

Create the App. Note its **App ID** or **Client ID** from the App's page; Vogt
accepts either. Then, under **Private keys**, choose **Generate a private
key** and keep the downloaded `.pem` file.

## 2. Install it

From the App's page, choose **Install App** and pick each account or
organisation whose repositories Vogt should read. Select all repositories, or
only the ones your Vogt projects link to.

One App can be installed on several accounts and organisations. Vogt discovers
the installation for each repository owner on first use, with
`GET /repos/{owner}/{repo}/installation`, then `/orgs/{owner}/installation` or
`/users/{owner}/installation`, and remembers it. A forge config that spans
several organisations therefore needs no per-owner settings.

If the App is not installed on an owner, Vogt remembers that for an hour. In
that hour it reads the owner's repositories with `github_token_file`, or
unauthenticated if no token file is set, and makes no App calls for that
owner. An installation added later is picked up when the hour is up.

To pin every request to one installation, set `github_app_installation_id`.
The installation ID is the number at the end of the installation's settings
URL (`.../settings/installations/<id>`).

**Publishing** (`forge.publish`) creates a repository before any repository
names an owner, so Vogt has to pick the installation:

- If `github_app_installation_id` is set, Vogt uses that installation.
- If it isn't set and the App has exactly one installation, Vogt finds it
  through `GET /app/installations` and uses it.
- If the App has several installations, set `github_app_installation_id`,
  or publishing refuses rather than guess.

On an organisation installation, Vogt creates the repository with
`POST /orgs/{owner}/repos`.

## 3. Configure Vogt

Deliver the private key as a file, the same way as the token file. Never
deliver it as an environment value. The two IDs are not secrets.

```toml
github_app_id = "123456"                 # or the Client ID, e.g. "Iv1.abc..."
github_app_private_key_file = "/run/secrets/github_app_key"
# github_app_installation_id = 7654321    # optional: pin one installation
# github_token_file = "/run/secrets/github_token"  # optional: notifications
```

Or, in the environment: `VOGT_GITHUB_APP_ID`,
`VOGT_GITHUB_APP_PRIVATE_KEY_FILE`, `VOGT_GITHUB_APP_INSTALLATION_ID`.

With Compose, mount the key as a secret:

```yaml
services:
  vogt:
    secrets: [github_app_key]
    environment:
      VOGT_GITHUB_APP_ID: "123456"
      VOGT_GITHUB_APP_PRIVATE_KEY_FILE: /run/secrets/github_app_key
secrets:
  github_app_key:
    file: ./secrets/github_app_key.pem
```

## How tokens are handled

Vogt signs a nine-minute RS256 JWT with the key (`iss` = the App ID or Client
ID) and exchanges it with `POST /app/installations/{id}/access_tokens` for an
installation token. It keeps each installation's token until five minutes
before it expires, which is about an hour. On a `401`, it mints a new token
and retries the request once. The rate budget and the ETag cache are kept per
installation, so one owner running low on budget doesn't pause the others.
Requests signed with the JWT, which look up installations and mint tokens,
have their own budget. Vogt checks it before sending one, so a low budget
stops those calls too.

## Switching back

Remove `github_app_id` or `github_app_private_key_file`, and set
`github_token_file`. Nothing else changes.

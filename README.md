# Bismuth CLI OSS

## Setup
First, get either an [Anthropic API key](https://console.anthropic.com/settings/keys) or an [OpenRouter API key](https://openrouter.ai/settings/keys) and export it to your environment as `ANTHROPIC_KEY` or `OPENROUTER_KEY` respectively.
Additionally, if you'd like to use vector search to improve Bismuth's internal code search: create a GCP project, enable the Vertex AI API, download a service account JSON, mount it into the `daneel` container in [`docker-compose.yaml`](./docker-compose.yaml), and set the `GOOGLE_APPLICATION_CREDENTIALS` variable as the path to the mounted JSON file.

Now you can `docker compose up`. This will bring up the main API and "Daneel", the main interactive chat server.

Finally, download (or build) the CLI:

```
VERSION=$(curl -fsS https://bismuthcloud.github.io/cli/LATEST)
TRIPLE=$(echo "$(uname -m | sed 's/aarch64/arm64/' | sed 's/arm64/aarch64/')-$([ "$(uname -s)" = "Darwin" ] && echo "apple" || echo "unknown")-$(uname -s | tr '[:upper:]' '[:lower:]')")
curl -fsSLo /usr/local/bin/biscli "https://github.com/BismuthCloud/cli/releases/download/v${VERSION}/bismuthcli.${TRIPLE}"
chmod +x /usr/local/bin/biscli
```

And log in to your local instance:

```
biscli login
```

> The default URLs can be used if running everything locally.

An account has been pre-created with the email `user@example.com` and password `user`. If you'd like to create other accounts or enable OAuth providers, you can log in to the keycloak administration page with `admin:admin`.

## Usage
Once you've started the services and logged in with the CLI, you can import a project:

```
biscli import /path/to/repo
```

And begin chatting:

```
biscli chat --repo /path/to/repo
```

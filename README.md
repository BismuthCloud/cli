# Bismuth CLI OSS

## Setup
First, get either an [Anthropic API key](https://console.anthropic.com/settings/keys) or an [OpenRouter API key](https://openrouter.ai/settings/keys) and export it to your environment as `ANTHROPIC_KEY` or `OPENROUTER_KEY` respectively.
Additionally, if you'd like to use vector search to improve Bismuth's internal code search: create a GCP project, enable the Vertex AI API, download a service account JSON, mount it into the `daneel` container in [`docker-compose.yaml`](./docker-compose.yaml), and set the `GOOGLE_APPLICATION_CREDENTIALS` variable as the path to the mounted JSON file.

Now you can `docker compose up`. This will bring up the main API and Daneel, the websocket chat server.

Finally, log in to this instance with the cli: `biscli login`.

> The default URLs can be used if running everything locally.

An account has been pre-created with the email `user@example.com` and password `user`. If you'd like to create other accounts or enable OAuth providers, you can log in to the keycloak administration page with `admin:admin`.

## Usage
Once you've started the services and logged in with the CLI, you can import a project:

```
biscli import /path/to/repo
```

And enter chat:

```
biscli chat --repo /path/to/repo
```

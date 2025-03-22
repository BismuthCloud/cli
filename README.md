# Bismuth CLI OSS

### Bismuth is your AI developer assistant. Ask it to add features or fix bugs, and it will propose changes right in your git repository.

![Video showing Bismuth being used to quickly change a website](/_doc/demo.mp4)

With some of the same technology that underpins [bismuth.sh](https://bismuth.sh), the Bismuth CLI can navigate and effectively work on large, complex code bases with ease.
It can create or edit files directly in your local repo, making small bug fixes or writing whole new features.
And it fits right into your development flow as a CLI app, letting you use it alongside any editor (or even remotely!).

When you import a project, Bismuth pre-processes your code breaking it into semantic chunks and indexing it so that it can be searched over instantly.

Then when you give a task to Bismuth, it searches through this index picking out relevant code files to give the LLM the specific context it needs to solve the problem allowing it to work on huge repositories without issue.

With that context, Bismuth uses the LLM of your choice to create or edit files, and returns a single diff to you for review all right in the terminal. When you accept it, the changes are added as a descriptive git commit to your repo making it easy to keep your code base clean.

![diff showing a change Bismuth is proposing](/_doc/diff.png)

## Setup
By default, Bismuth uses Anthropic's Sonnet 3.7 model and supports either Anthropic's API or OpenRouter.
To configure this, first get either an [Anthropic API key](https://console.anthropic.com/settings/keys) or an [OpenRouter API key](https://openrouter.ai/settings/keys) and export it to your environment as `ANTHROPIC_KEY` or `OPENROUTER_KEY` respectively.
Additionally, if you'd like to use vector search to improve Bismuth's internal code search: create a GCP project, enable the Vertex AI API, download a service account JSON, mount it into the `daneel` container in [`docker-compose.yaml`](./docker-compose.yaml), and set the `GOOGLE_APPLICATION_CREDENTIALS` variable as the path to the mounted JSON file.

Now you can `docker compose up`. This will bring up the main [API](/api) and [Daneel](/daneel), the main interactive chat server.

Finally, download (or build) the CLI itself:

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

An account has been pre-created with the email `user@example.com` and password `user` so you can get started quickly.
If you'd like to create other accounts or enable OAuth providers, you can log in to the [keycloak administration page](http://localhost:8543) with `admin:admin`.

## Usage
Once you've started the services and logged in with the CLI, you can import a project:

```
biscli import /path/to/repo
```

And begin using Bismuth:

```
biscli chat --repo /path/to/repo
```

# Daneel

Daneel is the real "brains" of Bismuth - the agentic code writing implementation plus supporting components (RAG, AST-based code chunking, etc.).

## Configuration

When standing up a self-hosted instance of Bismuth, the main [docker-compose.yaml](/docker-compose.yaml) will handle nearly all env setup for you.
However, it's recommended to set `GOOGLE_APPLICATION_CREDENTIALS` to enable vector search:

* `GOOGLE_APPLICATION_CREDENTIALS`: Path to GCP service account JSON key.

Additionally, the following are variables you might want to set:

* `ANTHROPIC_KEY`: Anthropic API key used for inference. Takes precedence over `OPENROUTER_KEY` and per-organization OpenRouter OAuth tokens.
* `OPENROUTER_KEY`: Openrouter API key used for inference. Takes precedence over per-organization OpenRouter OAuth tokens.

### Other Environment Variables

* `GIT_HOST`: host:port of the Bismuth API server where Git repositories are stored.
* `KEYCLOAK_URL`: Full URL of the Keycloak server including realm. Used on startup to pull OAuth verification tokens to validate session tokens.
* `REDIS_HOST`: hostname of the Redis server. Daneel uses Redis to cache state between agent execution steps.
* `POSTGRES_DSN`: DSN of the Postgres instance used by the API. Used for normal ORM operations like user, feature, project, organization, etc. lookups.
* `BISMUTH_GRAPH`: Path to store graph RAG database JSON files on disk.

* `BIG_MODEL`: Name/ID of the most advanced model to use, mainly for code writing.
* `MODEL`: Name/ID of the default model which is used for most other LLM tasks.
* `SMALL_MODEL`: Name/ID of a small model used for basic chat responses.

* `DANEEL_TRACE`: Path to store trace logs. Any module inside Daneel can call `trace_output` to record information about the currently active request to files in this path.

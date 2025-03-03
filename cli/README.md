# Bismuth

Bismuth is your AI developer assistant. Ask it to add features or fix bugs, and it will propose changes right in your git repository.

<!---
Demo video
-->

## Installation

It's recommended to use the `bismuthcli` Python package to install this CLI.

```
pip3 install bismuthcli && python3 -m bismuth install-cli
```

### Manual Installation

You can also manually run the commands that the installer package uses:

```
VERSION=$(curl -fsS https://bismuthcloud.github.io/cli/LATEST)
TRIPLE=$(echo "$(uname -m | sed 's/aarch64/arm64/' | sed 's/arm64/aarch64/')-$([ "$(uname -s)" = "Darwin" ] && echo "apple" || echo "unknown")-$(uname -s | tr '[:upper:]' '[:lower:]')")
curl -fsSLo /usr/local/bin/biscli "https://github.com/BismuthCloud/cli/releases/download/v${VERSION}/bismuthcli.${TRIPLE}"
chmod +x /usr/local/bin/biscli
```

Alternatively, binaries can be manually downloaded from the [releases](https://github.com/BismuthCloud/cli/releases) page.
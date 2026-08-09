# Security policy

Threadknot runs coding agents on your machine and serves their output to other
devices. That makes a handful of bugs unusually serious: anything that lets an
unauthenticated caller reach `/ws`, anything that lets a paired peer act as the
owner, anything that turns a shared browser or an artifact path into arbitrary
file access, and anything that leaks the LAN token, a mesh key, or a connector
key.

## Reporting a vulnerability

**Please do not open a public issue for a security bug.**

Use GitHub's private vulnerability reporting:
[**Report a vulnerability**](https://github.com/smith-network-solutions/threadknot/security/advisories/new).
It is private between you and the maintainers, and it gives us a place to
coordinate a fix and a CVE if one is warranted.

What helps most, in rough order:

- the version (Settings → About, or the release tag) and the platform
- which door the request came in by — the desktop window, a LAN browser, a
  paired peer, or a remote origin. The three listeners have deliberately
  different privileges and a report that doesn't say which one it used is very
  hard to reproduce (see [`docs/PROTOCOL.md`](docs/PROTOCOL.md))
- a minimal reproduction, ideally against `threadknot-headless`
- what you got that you shouldn't have

You will get an acknowledgement within **3 business days**. This is a small
project, so please treat that as a real commitment and not a service level: if
you don't hear back, ping the advisory thread.

We ask for **90 days** before public disclosure, or until a fix ships if that
comes sooner. We will credit you in the advisory and the changelog unless you'd
rather stay anonymous.

## Supported versions

The latest release gets security fixes. There are no long-term support branches.

## Scope

In scope: the desktop app, `threadknot-headless`, the LAN server, the
machine-to-machine mesh, the mobile companion, and the connector in
`src-tauri/src/connector.rs`.

Out of scope, because they are someone else's trust boundary:

- the agent CLIs themselves (`claude`, `codex`, `kimi`) — report those upstream
- what an agent chooses to do inside a folder you gave it access to. Threadknot
  is a way to run agents on your own machine; a thread with write access can
  write. That is the product, not a vulnerability
- findings that require an attacker who already has your LAN token, your user
  account, or physical access to an unlocked machine

## Things that are already known and documented

[`docs/REMOTE-ACCESS-SECURITY.md`](docs/REMOTE-ACCESS-SECURITY.md) is the threat
model, and it includes an honest list of accepted risks. A report that restates
one of those is welcome as an argument about the tradeoff, but it isn't news —
please say so up front so we can talk about the tradeoff rather than the bug.

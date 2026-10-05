# 0028. On a server, secrets are kept in one encrypted file in the store

Status: Accepted
Date: 2026-10-05

## Context
ARK keeps account tokens and saved API keys in a `Secrets` store (`ark-connect`). The only real
one was the OS keychain. On a server (ADR 0016: the app's binary serving with saved access
tokens, often in a container or a VM with a volume), that is the Linux kernel keyring: it lives
in kernel memory, so every restart, upgrade or new container loses the saved keys and
connections, while the store on the volume keeps the records that say they exist.

| Option | Why not chosen |
|---|---|
| The Secret Service (GNOME Keyring, KWallet) | needs a desktop session and a D-Bus daemon; servers have neither |
| Plain text in the store, mode 0600 | a copy of the volume (a backup, a snapshot) is a copy of every key |
| An external secret manager (Vault, a cloud KMS) | a service to run and a vendor to name; an app that has one can still bring its own `Secrets` |

## Decision
`ark_connect::FileSecrets`: all secrets in `<store>/secrets.enc`, one JSON map encrypted with
AES-256-GCM (`ring`, already in the dependency tree), a fresh nonce on every write.

| Question | Decision |
|---|---|
| The key | 32 bytes: `ARK_SECRETS_KEY` (base64) when the host sets it, so the key can stay off the volume; else `<store>/secrets.key`, made on first use |
| Files | Both mode 0600 on Unix, written whole through a temporary file and a rename |
| Missing or wrong key | `in_store` refuses at start with what to do: restore the key, or remove `secrets.enc` to start over. It never makes a new key over existing secrets |
| Errors | Name the files, never a secret or a key name |
| Choosing it | The app's: `ArkBuilder::secrets(Arc::new(FileSecrets::in_store(&root)?))`. `examples/writing-agent` picks it when serving with `--tokens`; on a person's computer the OS keychain stays the default |

## Consequences
- Keys and connections saved on a server survive restarts and image upgrades.
- With the key file next to the secrets, encryption protects a copy of `secrets.enc` alone, not
  a copy of the whole volume. A host that wants more sets `ARK_SECRETS_KEY` from its own secret
  store.
- One process writes the file at a time (an in-process lock). Two processes on one store could
  lose a write; one runtime per store is already the rule (ADR 0016).
- Losing the key loses the secrets: they can be entered again, nothing else is lost.

## Check
`crates/ark-connect/src/secrets.rs` tests: secrets survive a reopen, the file holds no
plaintext, both files are 0600, a missing or wrong key is refused without printing a secret.

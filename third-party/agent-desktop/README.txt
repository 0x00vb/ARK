agent-desktop by Vercel Labs
https://github.com/vercel-labs/agent-desktop
Version 1.0.2, commit a47ed664e0db00d1d4df364eb3fe8a5599882f3c
The core and macOS adapter are linked without changes to their source.
The original Apache-2.0 license is included beside this file.

macos/ is the exact macOS crate from the pinned revision, with a standalone workspace
manifest and one build-script change. VENDOR.json records every upstream file digest and
the local changes. agent-desktop-core retains its original exact Git identity.

The native Objective-C sources use the package-specific
AGENT_DESKTOP_MACOS_DEPLOYMENT_TARGET (default10.15, as upstream) instead of inheriting the
final application's MACOSX_DEPLOYMENT_TARGET. ARK Desktop still targets macOS11.0. All
-Wall, -Wextra and -Werror flags remain. This restores the library's source availability
range and changes which deprecation diagnostics apply. It does not remove the deprecated
fullPathForApplication name resolver or change its runtime behavior. The arm64 object
still records an11.0 minimum, as required by the architecture. See release evidence.

# Code signing setup (owner actions)

No code depends on this yet; start it now because approvals take days to weeks.

## Apple (macOS, Developer ID + notarization)
1. Enrol the organisation in the Apple Developer Program (needs a D-U-N-S number for an organisation; allow 1-2 weeks).
2. In the portal create a **Developer ID Application** certificate (and **Developer ID Installer** if shipping a `.pkg`). Export it with its private key as a `.p12`.
3. Create an app-specific password (or an App Store Connect API key) for `notarytool`.
4. Store as GitHub Actions secrets: `APPLE_CERT_P12_BASE64`, `APPLE_CERT_PASSWORD`, `APPLE_TEAM_ID`, `APPLE_ID` + `APPLE_APP_PASSWORD` (or API key id/issuer/key).
5. Release flow later: `codesign --options runtime --timestamp`, `xcrun notarytool submit --wait`, `xcrun stapler staple`.

## Windows (Authenticode)
1. Choose a signer. Since 2023 OV/EV keys must live on hardware or a cloud HSM; prefer **Azure Trusted Signing** (cheap, CI friendly) or a cloud-HSM OV/EV certificate.
2. Complete identity validation for the organisation (days to weeks).
3. Store the credentials as secrets (Azure: tenant id, client id, client secret, endpoint, account, profile name).
4. Release flow later: `signtool sign /fd SHA256 /tr <timestamp-url> /td SHA256` on every `.exe`.

Track the status of each step on WH-2 until both identities are issued.

# Network speakers verification: 2026-11-15

Feature: Network speakers (a LAN web page that plays the PC audio on phones, PCs and TVs). Branch `main`, commits 9054ac7..0198381. Automated results are in the task report; this file tracks the real-device matrix.

Rows marked **Not tested yet** have no result. Nothing here is estimated.

## Device matrix

| Device / browser | Result | Latency after Calibrate | Notes |
|---|---|---|---|
| Phone on the installed app, QR join (2026-11-15) | **Played.** Scanned the QR code with a phone on the same Wi-Fi; audio played. | Not recorded | Device model and browser not recorded. First attempt failed, see "Firewall finding" below. |
| iPhone Safari | Not tested yet | | Check: QR join, Tap to play, sound with the ring/silent switch on silent, audio stops or continues when the screen locks, Calibrate result, "Weak Wi-Fi" option. |
| Android Chrome | Not tested yet | | Check: QR join, Tap to play, behaviour with the screen off, Calibrate result, dropouts. |
| Second PC (Chrome or Edge) | Not tested yet | | Check: typed address, Allow/Deny request on the host PC, remembered device on re-open, Calibrate result. |
| Smart-TV browser (Samsung, LG or Android TV) | Not tested yet | | Check: page loads and is readable from a distance, Tap to play works with the remote, audio plays, which TV model and browser version. |
| 30-minute run on one phone | Not tested yet | | Check: drift (does the delay need re-tuning after 30 min), dropouts or underruns, the phone staying connected, the PC speakers staying clean. |

## Firewall finding (2026-11-15)

The first QR scan on the installed app did not connect. Cause: Kaspersky Internet Security was the active firewall and the Windows network profile was Public. It worked after:

- allowing the Windows firewall prompt on Private networks,
- setting the network to Trusted in Kaspersky (or allowing `syncsound-desktop.exe`),
- optionally switching the Windows network profile to Private.

The README "Phone can't connect?" section and the website FAQ cover this in generic terms (any third-party antivirus firewall, guest Wi-Fi, client isolation).

## Screenshot method

`site/assets/06-network.webp` and `docs/screenshots/06-network.png` show the device page in its idle "Tap to play" state. They were rendered by headless Edge from a scratch copy of `crates/syncsound-engine/src/net/page/` at 500x844 CSS px at 2x (the layout width Edge headless allows), cropped to 390 wide. The page source was not changed. It is not a capture of a real phone.

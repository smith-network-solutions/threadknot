# Threadknot updates

Client-facing update notes. This file is embedded into the app at build time
(see src-tauri/build.rs) and shown when you click the version number in the
sidebar footer. Keep entries in plain language a user understands: what
changed, and where to find it. No internal jargon, hashes, or refactor notes.

Format (parsed by build.rs, keep it exact):
  "## v<version> · <YYYY-MM-DD>" starts a release, "- " lines are its bullets.

## v0.1.64 · 2026-08-11

- **Updating is one click again, and Windows can finally do it.** Under Settings → updates, the button on a machine that can finish the job now reads **pull, build & restart**: it fast-forwards the checkout, rebuilds, and relaunches itself into the new build, showing each stage as it goes. The build keeps running if you close the window or the tab that started it. Windows machines could previously only pull, and had to be rebuilt and reopened by hand.
- **Nothing restarts out from under working threads.** If any thread starts a turn or asks for approval while the build is running, the update stops after building and leaves the ordinary **restart now** button for when they finish. Rebuilds and restarts that end without changing anything now say so on the card instead of finishing silently.

## v0.1.32 · 2026-08-10

- **New About section, at the foot of Settings.** What Threadknot is, the exact build you are running (version, build date, commit and machine), the Apache-2.0 licence with links to the source, the third-party notices and the security policy, and the credits. It rewards reading all the way to the bottom.

## v0.1.31 · 2026-08-09

- Claude chats now start correctly on Windows when Threadknot's browser tools are enabled. The generated MCP configuration uses a short-lived file there instead of inline command-line JSON, avoiding Windows quote mangling and removing the file when Claude exits.
- **Your models become the characters.** Under the Retro-Tech skin, sidebar thread cards and the thread hover preview are full fighter plates: a framed character portrait for the model driving the thread, the title in pixel type, and a vitality strip that turns red and blinks when a thread needs you. An original set of pixel characters ships built in (one per model family), and you can replace any of them under Settings → appearance → model portraits (any image works; it is downscaled automatically), with a per-agent default and one-click reset.
- **The Arcade theme is now Retro-Tech, and it reaches the whole app.** Files, Git, Artifacts, and the Browser workspace pick up the cabinet look: pixel-face labels and tabs, framed rows, lit active states. The reviewer dialog got more breathing room, sidebar cards read like fighter cards with a bezeled portrait and a framed turn counter, the settled shelf goes attract-mode dim, and hovering a project icon in the rail pops an enlarged badge with its name.
- **Starting a review debate announces the fight.** ROUND 1 slams across the screen when the reviewers seat, and every following back-and-forth round calls out ROUND 2, ROUND 3, and so on. Retro-Tech only.
- **The terminal goes green phosphor.** With Retro-Tech on and no custom terminal settings, the terminal turns classic green-on-black with a blinking block cursor and scanlines. Any terminal preference you have set yourself is always respected instead.
- **New Skins area under Settings → appearance.** See what a skin looks like before applying it (click Preview for screenshots), and switch any part of it off: usage bars, review and rounds, sidebar cards, terminal, or workspace panels, so your own setup survives the skin. Crafted themes can be exported to a file, imported from one, and shared through the community marketplace.
- **Appearance settings are easier to scan.** Each section now has a clear break, in every theme.

## v0.1.21 · 2026-08-09

- **The Arcade theme's usage meter is now a retro arcade health bar.** With the Arcade theme on (Settings → appearance), each subscription window draws as a yellow vitality bar that red damage eats from the left as you spend it. The last sliver strobes when a window is nearly gone, providers square off around a VS badge in the usage popover, and a provider that fails to report its usage is marked K.O. Every other theme looks exactly as before.

## v0.1.176 · 2026-08-08

- **Quick Chats give one-off questions their own home.** Open the pinned Threadknot tile in rail mode (or **quick chats** in the sidebar), start a chat without choosing a workspace, and return to its recent or settled conversations later. Each one starts read-only in its own private scratch folder; choose Full access only when you want the agent to work on the computer itself.
- **Paste now works in the Browser workspace.** Click a field and use Ctrl+V or Cmd+V as usual, or tap the clipboard button on a phone. If mobile browser permissions block clipboard access, Threadknot gives you a box where the phone’s own Paste command works.
- **Push notifications work again.** Every notification had been failing since the rename: phones paired with the old Armada app left push tokens behind, and the notification service rejects a batch that mixes two apps — so one stale token stopped notifications reaching *every* phone, including ones running the current app. Threadknot now sends each app's phones their own batch.
- A phone paired more than once no longer gets the same notification two or three times.
- **Notifications now tell you what actually happened.** Finished turns preview the agent's final answer, questions show the question, approvals show what needs permission, and errors show the reason. Desktop, browser, and phone alerts use the same wording; choose **status only** under Settings → notifications if you do not want message text on a lock screen.
- **Connecting a machine to the hosted relay is now one button.** Settings → reachable from anywhere → **connect this machine** opens app.threadknot.ai, you sign in and press Approve, and the machine picks it up by itself — there is nothing to copy, and no code to carry to your phone. Pasting a token still works and is tucked under "or paste a token from the console" for scripted setups.
- **Your trial now starts when you connect your first machine, not when you sign up.** Signing up to look around no longer quietly burns trial days before you have used anything.
- **Threadknot warns you before the trial ends.** Settings shows the days remaining once you are inside the last week, and says what happens when it runs out: sessions already running are never cut off, only new ones stop.
- **Opening your machine's web address in a new browser now tells you how to get in.** It used to load the app and sit on "offline — retrying…" with no explanation. Now it asks for the one-time code from Settings → pair a phone → from anywhere, and lets you in as soon as you type it.
- Fixed **Subscribe** on the billing page, which failed with a deserialization error instead of opening Stripe Checkout.

## v0.1.175 · 2026-08-08

- **Connections between your machines are now encrypted.** Each machine has its own certificate, exchanged when you pair, so a machine is recognised by its key rather than by its address — and nothing sensitive travels in the clear on your network any more. Machines paired before this update show **update needed** in Settings → machines instead of connecting: update Threadknot on that machine, then pair the two again. Nothing else changes, and your chats and projects are untouched.
- **Pairing two machines no longer sends either one's master token.** The two machines prove they know it instead, and swap credentials that only work on that one link and can be replaced without re-pairing everything.
- **A phone only does on another machine what you allowed it to do here.** Previously a request sent on to a paired machine arrived there as that machine's owner, so a phone you had not given terminal access to could still open one somewhere else in your fleet. Now the permissions you set travel with the request.
- **Large downloads and video previews no longer slow the app down.** Files, attachments and recordings stream instead of being loaded whole, so opening a multi-gigabyte recording no longer uses gigabytes of memory, and scrubbing a video fetches only the part it needs.
- Downloads now show real progress, because the file size is sent up front.
- A machine or phone that stops responding can no longer make Threadknot use more and more memory waiting for it. It is disconnected instead, and reconnects on its own.

## v0.1.174 · 2026-08-06

- Claude chats can now launch with Claude in Chrome. Choose **Enabled** under **Claude in Chrome** beside the message box before sending, and Threadknot starts that session with Chrome control turned on.
- Returning to the mobile app now reconnects and refreshes the chat already on screen, so messages that arrived while the phone was asleep appear without switching chats.

## v0.1.169 · 2026-08-03

- **Zoom now grows the conversation, not the controls.** Zooming (Ctrl+wheel or Settings → Appearance) scales only the message text, up to a new 2x maximum; the message box, thread header, and terminal stay their true size. Your reading position holds steady through zoom changes.
- **Size the message box your way.** Settings → Appearance → composer: width (cozy, wide, or full), text size (12 to 18px), and a compact density option. It stays exactly that size at any zoom.

## v0.1.166 · 2026-08-02

- The settle tick now asks "Settle this chat?" in a small popup before parking anything, so a stray click can no longer settle a chat by accident. Bringing a chat back from the shelf stays one click.

## v0.1.160 · 2026-07-31

- Open chats no longer blink, jump to a loading screen, or close the background-agent panel when another Threadknot machine reconnects or sends a peer-presence update.
- Sending while an agent is working no longer means Stop: Claude and Codex take the note during the active turn, while Kimi queues it for the next turn boundary. Stop remains a separate red button beside Send.

## v0.1.159 · 2026-07-31

- Claude background agents stay visible in the pinned agent panel until they actually finish. The launch confirmation no longer makes their cards disappear while Claude is still waiting on them.

## v0.1.158 · 2026-07-31

- Sidebar search now finds words inside chat messages and other thread content, not just chat titles.
- Kimi Code now keeps its progress narration beside the tool call it describes instead of hiding those updates and combining them above the final answer.
- Kimi child agents now show their delegated brief, type, elapsed time, live thinking and tool activity, and final result. Older Kimi backends also get a visible child-agent timer instead of an unexplained **Agent · Working…** row.

## v0.1.157 · 2026-07-31

- Archives now get the whole Settings page instead of a small box: the list fills the pane and scrolls, so long archive histories are easy to work through.
- Archives are organized by machine. A row of machine chips at the top shows this machine first (selected by default) and every connected machine with its online dot and how many archives it holds - pick one to browse the archives stored there. A machine that is offline says so, with a retry button.
- You can restore and delete archives on any of your machines, not just the one you are sitting at. Restoring a chat reactivates it on the machine that owns it and takes you straight into that chat - Settings closes and the conversation opens, ready to continue.
- The archive storage location row now only appears for the machine you are on, since each machine manages its own archive folder.

## v0.1.155 · 2026-07-31

- **Place your wallpaper exactly where you want it.** In the theme studio, zoom the background image from 1x to 3x (slider, mouse wheel over the preview, or double-click to reset) and drag it around the preview to position it: the real background follows live as you drag, and the placement saves with the theme.

## v0.1.149 · 2026-07-31

- **41 fonts, previewed properly.** 27 interface fonts and 14 code fonts, and the font menus now show every option in its own typeface with a sample line. Fonts download on demand and cache, so the app stays fast.
- **The theme studio grew up.** Crafting a theme is now a proper two-column editor: name, base palette and accent on the left with a live wallpaper preview under the dim slider, and the color work on the right with all ten palette slots visible as a swatch grid.
- **AI color schemes.** Give your theme a wallpaper and press "AI color scheme": Claude looks at the image on your own machine and designs a matching palette (accent, backgrounds, text, even a name suggestion) in about half a minute, ready for your tweaks. The instant "match colors" option is still there.
- Fixed the app's fonts silently falling back in release builds: the security policy never allowed the font host, which also affected the default look.

## v0.1.145 · 2026-07-30

- **Make Threadknot look like yours.** Settings → Appearance is now a theme studio: six built-in palettes (dark, midnight, slate, carbon, light, solar), nine accent colors or any custom color, and separate interface and code fonts (the terminal follows your code font, and has its own font row too).
- **Craft your own themes.** Create a theme from any palette, tune its ten color slots, and give it a background image with a dim slider that keeps text readable. Or load a wallpaper and hit "match colors from image": Threadknot pulls the dominant colors out of the picture and builds the whole palette to match, ready for your tweaks.
- Crafted themes are saved on the machine (not in the browser), show as cards in the gallery next to the built-ins, preview live while you edit, and come back exactly as you left them after a restart.

## v0.1.140 · 2026-07-30

- Windows dictation now picks the device that is actually a microphone. It previously recorded from the first audio device Windows listed, which on many laptops is a camera's audio input: a source that produces perfect silence, making the mic button "work" but never hear anything.

## v0.1.139 · 2026-07-30

- The Download button in Artifacts and Files now works on desktop: it opens a normal save dialog (filename pre-filled) and saves the file where you choose, including artifacts that live on another machine. It previously did nothing outside the phone/browser.

## v0.1.138 · 2026-07-30

- **Dictation now shows you it's working.** After you click the mic off, a spinning circle and "Transcribing…" appear in the corner of the text box until your words land, so the pause is never a mystery. Your typed text shuffles aside while it's there and moves back afterwards.

## v0.1.137 · 2026-07-30

- **Dictation works on Windows now**, alongside macOS and Linux. Windows has no idea of a "default" microphone, so Threadknot asks it for the list and records the first one it reports. If that turns out to be the wrong input (a webcam mic instead of your headset), set `THREADKNOT_MIC_DEVICE` to the name of the one you want.

## v0.1.136 · 2026-07-30

- **You can talk to the composer now.** There's a mic button next to the paperclip: click it, say what you want, click it again, and your words drop into the box where you can edit them before sending. Press Esc while it's listening to throw the clip away. Dictation runs entirely on your own machine, so nothing is sent anywhere, and it needs Whisper installed (`pip install -U openai-whisper`) — until it is, the mic button sits greyed out and says so. Clips are capped at two minutes.
## v0.1.133 · 2026-07-29

- Claude effort now starts on **Default**, showing Anthropic's effective level for the selected model (for example, **Default (High)**). Default leaves the effort choice to Claude and sends no override, while choosing Low, Medium, High, Extra High, or Max still applies that exact level.

## v0.1.125 · 2026-07-29

- Chats you are done with can now be **settled**: they collapse into a per-project "settled" shelf at the bottom of the section instead of adding to one endless scroll. Hover a chat and click the tick, or use the chat menu. Settling is shared across your devices, so parking a chat on your phone parks it on the desktop.
- Quiet chats settle themselves. By default anything untouched for 3 days moves to the shelf; change or turn that off in Settings → Appearance. A chat that is working, waiting on you, unread, or currently open is never settled, and any new activity brings it straight back.
- Chats in a project now keep a fixed order instead of re-shuffling every time an agent does something, so a row stays where you last saw it.
- **Projects stay where you put them too.** They no longer jump to the top when an agent finishes something — you set the order yourself by dragging: grab a project's icon on the rail, or the grip that appears at the left of its header in the other layouts, and drop it where you want. This replaces dragging the header itself (added in v0.1.124), which now still means "open this project in its own window". On a phone, hold a project for a moment first, then drag. The order is remembered per device.
- Tapping a project on the rail now opens something in it, instead of leaving the chat from the project you just left filling the screen. You land on the chat that needs you, or failing that the one that is working, or failing that the most recent one.
- Hovering a project icon no longer blows it up into a large floating copy of the picture, and the project you are in keeps the same shape as the others on the rail instead of turning into a circle.
- A chat that is merely busy now sits back visually; the sidebar saves its brightness for chats that actually want you — a pending approval or an unread finish.
- **Four ways to show projects**, in Settings → Appearance → sidebar. *All open* is the layout Threadknot has always had (every project expanded at once). *One open* keeps all the project headers visible but only ever shows one list of chats. *Picker* drops the headers entirely: you choose a project from a bar at the top and it gets the whole sidebar. *Rail* puts a column of project icons down the left edge, Discord-style — one tap to switch, with every project's unread badge always in view. On a phone, picker and rail each turn three screens of scrolling into one.
- Sidebar rows are more compact on phones — noticeably more chats visible without scrolling, and the buttons are still a full finger-width to hit.
- On phones: long-press a chat for its actions (rename, settle, archive, delete), which were previously behind an invisible button.

## v0.1.124 · 2026-07-28

- Hover a chat in the sidebar to see a live summary card: which machine and model it runs on, when it was active, and the last exchange. Works for chats on your other machines too. Workspace names show their machines and folders on hover.
- New funnel button next to the sidebar search: switch between List, Compact, and Cards layouts, use larger workspace names, hide timestamps, and keep this machine's workspaces pinned on top.
- Drag the sidebar's right edge to resize it; double-click the edge to reset.
- Star chats and workspaces (hover a row or use its menu) to keep favorites at the top. Stars follow you across machines.
- Drag workspace headers up or down to reorder the sidebar; dragging one onto the work area still opens it in its own window.
- The + button on a workspace now always shows where the new session will run, and can add a new project folder on any machine and start the session there.
- Hermes agents show a live Online/Offline dot everywhere they appear (sidebar, chats, message box, Settings), refreshed every 20 seconds; offline agents show how long they have been down.
- Sidebar menus can be driven from the keyboard: arrow keys move, Escape returns to where you were.

## v0.1.112 · 2026-07-28

- Pasting an image into the message box now attaches one copy instead of two.

## v0.1.108 · 2026-07-28

- New Codex chats now start with GPT-5.6 Sol at High reasoning.
- Finished background chats now show an unread dot in the sidebar until you open them, including after a refresh.

## v0.1.107 · 2026-07-27

- Every user and agent message now shows its local send time, chat headers show when the thread started and its latest-message span, and completed-turn dividers show how long the task took.

## v0.1.106 · 2026-07-27

- Chats now stay where you were reading when work finishes in the current chat or another one, while chats already at the bottom continue following new output.
- Published video artifacts now play directly in the desktop Artifacts viewer.

## v0.1.102 · 2026-07-27

- K3 now offers its real **Low**, **High**, and **Max** effort levels in the composer, with High selected by default.
- Automatic post-reboot continuation now uses one short message: “Threadknot Rebooted - Continue where you left off.”

## v0.1.101 · 2026-07-27

- New agent: **Kimi Code** — use K3 in Threadknot through your Kimi subscription, with streaming replies, tools, approvals, plan mode, images, browser tools, interruption, and resumable chats. Authenticate once with `kimi login`; Threadknot never needs API credits or an API key.

## v0.1.100 · 2026-07-26

- New agent: **Claudex** — Claude Code's harness (its tools, permissions, plan mode and subagents) driven by a different model. Set one up in Settings → Agents → claudex profiles; the form comes pre-filled for GPT-5.6 Sol through a local bridge on your ChatGPT subscription, and "test" starts the bridge and checks it answers.
- Each Claudex profile is kept entirely separate from your real Claude: its own settings folder, its own chat history, and its own login. A Claudex chat can never quietly run on, or bill, your Claude plan.
- Claudex chats show the profile's real context window, and picking a different profile starts a fresh session rather than reusing the previous one.
- Usage for a Claudex chat is billed by whatever its bridge is signed into, so the sidebar's Claude meter does not track it.

## v0.1.99 · 2026-07-26

- Long artifact names in chat now shorten cleanly instead of running underneath the Open button.

## v0.1.92 · 2026-07-26

- Fixed the browser's startup settings never actually reaching Chrome, which quietly disabled several behaviours including support for embedded (cross-site) frames.
- Copying an artifact now hands over a file with its real extension, so it opens in whatever you paste it into.

## v0.1.89 · 2026-07-26

- Browsers can stay signed in. Create a signed-in browser in Settings → Browser logins, attach it to a chat from the Browser tab, and sign in yourself in that pane — the login sticks for future chats and after restarts. Threadknot keeps the session, never your password.
- A signed-in browser is fenced in: it can only visit the sites you listed, it can't run arbitrary scripts, and it's limited to one chat at a time. Everything else keeps using a throwaway browser as before.
- Signing a profile out erases its stored session, and a signed-in chat's browser can't be opened from a paired phone or a shared link.

## v0.1.86 · 2026-07-25

- Clicks no longer disappear into cookie banners: if something covers the button an agent is aiming at, the action stops and says what is in the way instead of quietly clicking the overlay.
- The network list keeps the page's own request, so an agent can see the status of the page it is looking at, and can now read a failed request's headers alongside its body.

## v0.1.81 · 2026-07-25

- Agents can now complete real web flows: pressing Enter actually submits a form, typing fires real keystrokes so search boxes and editors respond, and everything inside an iframe — checkout fields, embedded sign-in, third-party widgets — is finally visible and clickable.
- The agent reads the page as a proper outline, so it can tell repeated buttons apart by the row or section they belong to, and far more of a long page fits before anything is cut.
- Agents can save screenshots — the viewport, a full page, or a single element — and share them with you as evidence, and files the browser downloads now land in a real folder you can open.
- Better error reading: failed page loads, blocked requests, and security errors now show up alongside console messages, and the agent can read the response body behind a failed request. Console entries reset when you navigate, so they always describe the page you are on.
- Agents can resize the browser to check responsive layouts, and scrolling reports where it landed and whether the bottom is reached.
- Browsers clean up after themselves: leftover browser data from earlier runs is removed at startup, and unused browser sessions close on their own instead of holding memory forever.

## v0.1.68 · 2026-07-25

- The Browser workspace is back. It now stays connected when you switch between Files, Git, Artifacts, Browser, and Terminal instead of breaking the live page.
- Watch the agent work in the shared browser: its cursor, target outline, current action, failures, and reconnect-safe activity history are visible while you can take over the same page at any time.
- Browser control is far more reliable: agents can inspect a readable page structure, act on stable element references, fill whole forms, hover, drag, manage tabs and popups, upload project files, answer page dialogs, wait for page state, and inspect console or network failures.
- Browser sessions now recover from a crashed engine, keep the address bar synchronized through links and app-style navigation, and include an explicit restart control when you want a clean isolated session.

## v0.1.66 · 2026-07-24

- Claude chats now recover automatically when a connection stalls before work begins. Stop also retires the old provider process, so the next message starts with a clean connection instead of reusing a stuck one.
- Claude Opus 5 is now available in the model picker, replacing Opus 4.8. Existing Opus 4.8 chats, schedules, and saved new-chat preferences move to Opus 5 automatically, with its fixed 1-million-token context window.

## v0.1.58 · 2026-07-23

- Workspace pictures now accept source images up to 10 MB and automatically resize and compress them, instead of incorrectly rejecting them at the much smaller profile-avatar limit. (Right-click a workspace → Add workspace image)

## v0.1.50 · 2026-07-23

- Background agents are now visible while they run: each shows a live status card in the chat, and a pinned indicator above the composer counts how many are still working — tap it for a popover with every agent's status and result.
- Fewer notifications: a chat that launches background agents now signals "done" just once, when everything has truly finished, instead of pinging as each agent wraps up.

## v0.1.49 · 2026-07-23

- Machine profiles (names, pictures, and colors) now sync automatically across your fleet, machine to machine. Edit any machine's profile from any machine and it updates everywhere, with the most recent edit winning. There is no admin machine to elect and nothing to approve. (Settings → Machines)
- Editing another machine's card gives you two choices: "Set profile" changes that machine everywhere, or "Override locally" changes only how it looks on this machine. (Settings → Machines)

## v0.1.48 · 2026-07-23

- Chats that were actively working now resume automatically after Threadknot restarts. Approval and question cards still wait for your answer.
- Mobile: touch and hold a workspace or Hermes agent to open the same actions menu as right-click, including workspace images.
- Mobile terminals now have a Paste key, with native clipboard support in the Threadknot app and a manual fallback for LAN browsers.

## v0.1.45 · 2026-07-23

- Hermes chats now remember the whole conversation. Replies keep the context of everything said earlier in the thread. (Hermes Agents → any chat)
- The version number in the bottom-left corner is now live. Hover it to see when the update went live; click it to open these update notes.
- Updates now version themselves automatically with every change shipped.

## v0.1.41 · 2026-07-22

- Machine profiles overhauled: profile cards, a Customize Profile popup, and an accent color picker for every machine. (Settings → Machines)
- Give machines and Hermes agents a photo. Avatars show a hover preview and a crop tool when you upload. (Settings → Machines → Customize Profile)
- Zoom now scales just the work area, with a live zoom label and Ctrl + scroll-wheel shortcuts.
- Hermes agents get their own sidebar view, accept images in messages, and nudge you with a dot when a chat needs attention. (Sidebar → hermes agents)
- Pairing machines now syncs your entire workspace catalog automatically across all of them.
- Work on other machines as if local: git, terminals, and files stream through the mesh from any paired machine.
- Starting a new session lets you pick which machine runs it.
- Settings moved to a full-screen page instead of a small popover. (Sidebar → gear icon)
- Refreshed logo, app icons, and branding.
- New illustrated user guide for non-technical teammates. (docs → user guide PDF)
- Mobile: file downloads open the native share sheet, and unlock is remembered for 5 minutes.

## v0.1.22 · 2026-07-21

- Mobile companion app: pair your phone and get push notifications when an agent needs your input.
- Rename, archive, and restore sessions from the sidebar. (right-click a session)
- Light theme, plus appearance and terminal preferences. (Settings → Appearance)
- Claude Opus 4.8 can now run with a 1-million-token context window. (model picker)
- Break a project out into its own window. (drag it out, or right-click → break out)
- Terminals remember their folder and no longer blank out when switching tabs.
- Windows: agents launch natively and much more reliably.

## v0.1.11 · 2026-07-20

- First working release: run Claude Code and Codex side by side across your projects.
- Files, Browser, and Terminal tabs in every workspace.
- Attach images to prompts (paste or pick); a meter shows how full the conversation's context is.
- Structured question forms, polished code/markdown rendering, and mid-conversation agent switching.
- Official provider logos, full model lists, and per-model reasoning effort settings.

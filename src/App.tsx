import { useEffect, useMemo, useReducer, useRef, useState } from "react";
import { discoverServer, pickDirectoryNative } from "./lib/discovery";
import {
  advertiseSoloWindow,
  detectSoloProject,
  hasSoloWindow,
  openProjectWindow,
} from "./lib/solo";
import {
  chime,
  getNotifyPrefs,
  invalidateNotifyPrefs,
  showSystemNotification,
  vibrate,
  wantsWorkspace,
} from "./lib/notify";
import {
  initNativeBridge,
  isNativeShell,
  postToNative,
  setNativeNavigationHandler,
  setNativeResumeHandler,
} from "./lib/native";
import { isWindowFocused, startFocusTracking } from "./lib/focus";
import { initZoomHotkeys } from "./lib/hotwheel";
import { installExternalLinkHandler } from "./lib/links";
import { getSidebarPrefs } from "./lib/appearance";
import type {
  Access,
  Agent,
  ClaudexProfileInput,
  EventFrame,
  OutgoingAttachment,
  ReviewerPersona,
  ThreadSettings,
} from "./lib/protocol";
import { HERMES_HOME_PROJECT_ID } from "./lib/protocol";
import { isAgentVisible } from "./lib/agentVisibility";
import { ThreadknotClient } from "./lib/ws";
import {
  defaultDraft,
  effortForModel,
  findThread,
  forgetNewThreadSettings,
  initialState,
  loadThreadAttention,
  LS_THREAD_ATTENTION,
  persistThreadAttention,
  lastThreadKey,
  projectOwner,
  remoteMachineId,
  rememberNewThreadSettings,
  reducer,
  StoreContext,
  threadSettled,
  workspaceIdForProject,
  type Action,
  type AppState,
  type ThreadknotActions,
} from "./state/store";
import { Sidebar } from "./components/Sidebar";
import { ThemeSync } from "./components/ThemeStudio";
import { MainSplit } from "./components/MainSplit";
import { DirPicker } from "./components/DirPicker";
import { SchedulesPanel } from "./components/SchedulesPanel";
import { AvatarCropHost } from "./components/AvatarCropModal";
import { PullToRefresh } from "./components/PullToRefresh";

function makeActions(
  client: ThreadknotClient,
  dispatch: React.Dispatch<Action>,
  getState: () => AppState,
): ThreadknotActions {
  /** The machineId to route a thread's RPCs by (undefined = local). */
  const routeFor = (threadId: string): string | undefined =>
    remoteMachineId(getState(), findThread(getState(), threadId)?.machineId);

  /** Route by a project's owning machine (from workspace membership). */
  const projectRoute = (projectId: string): string | undefined =>
    remoteMachineId(getState(), projectOwner(getState(), projectId));

  /** Route a repoId via the project its record belongs to. */
  const repoRoute = (repoId: string): string | undefined => {
    const s = getState();
    for (const pid of Object.keys(s.git)) {
      if (s.git[pid].some((r) => r.id === repoId)) return projectRoute(pid);
    }
    return undefined;
  };

  /** Route a termId via the project whose tab list holds it. */
  const termRoute = (termId: string): string | undefined => {
    const s = getState();
    for (const pid of Object.keys(s.terminals)) {
      if (s.terminals[pid].some((t) => t.id === termId)) return projectRoute(pid);
    }
    return undefined;
  };

  const route = (machineId: string | undefined) =>
    machineId ? { machineId } : {};

  const refreshThreads = async (projectId: string, machineId?: string) => {
    const { threads } = await client.request(
      "thread.list",
      machineId ? { projectId, machineId } : { projectId },
    );
    dispatch({ type: "threads", projectId, threads });
  };

  const refreshWorkspaces = async () => {
    const { workspaces } = await client.request("workspace.list", {});
    dispatch({ type: "workspaces", workspaces });
    // Remote roots' threads come via the owner (proxied); best-effort — an
    // offline machine's threads just keep their last-known state.
    const local = getState().hello?.machineId;
    for (const w of workspaces) {
      for (const m of w.members) {
        if (local && m.machineId !== local) {
          void refreshThreads(m.projectId, m.machineId).catch(() => undefined);
        }
      }
    }
  };

  const refreshPeers = async () => {
    const { peers, discovered } = await client.request("peer.list", {});
    dispatch({ type: "peers", peers, discovered });
  };

  const listThemes = async () => {
    const { themes } = await client.request("theme.list", {});
    dispatch({ type: "customThemes", themes });
    return themes;
  };

  /**
   * Re-pull every remote member's thread list from the workspaces already in
   * state.
   *
   * A machine that was offline answers `thread.list` with an error, so a client
   * that booted during the outage has nothing for it and renders "no threads
   * yet" — while a client that was open beforehand keeps showing its last-known
   * list. Reconnecting has to actually refill the newly-reachable machine, or
   * whether you see a peer's chats comes down to when your tab happened to
   * load.
   */
  const refreshRemoteThreads = async () => {
    const s = getState();
    const local = s.hello?.machineId;
    if (!local) return;
    await Promise.all(
      s.workspaces.flatMap((w) =>
        w.members
          .filter((m) => m.machineId && m.machineId !== local)
          .map((m) =>
            refreshThreads(m.projectId, m.machineId).catch(() => undefined),
          ),
      ),
    );
  };

  const refreshProjects = async () => {
    const { projects } = await client.request("project.list", {});
    dispatch({ type: "projects", projects });
    // The sidebar renders workspaces above projects — keep both in step.
    void refreshWorkspaces().catch(() => undefined);
    await Promise.all(
      projects.map((p) =>
        refreshThreads(p.id).catch(() => undefined),
      ),
    );
  };

  const refreshGitRepos = async (projectId: string) => {
    const { repos } = await client.request("git.repos", {
      projectId,
      ...route(projectRoute(projectId)),
    });
    dispatch({ type: "git", projectId, repos });
    return repos;
  };

  /** Per-machine request generation, so a slow `archive.list` can't clobber a
   *  newer one. Keyed by the same storage key the list is committed under. */
  const archiveGen = new Map<string, number>();

  const refreshArchives = async (machineId?: string) => {
    // Archives are machine-scoped: `archive.list` routes to the owner when
    // `machineId` names a remote machine, and the result is keyed under the
    // owning machineId (the local id when omitted). Bail if we have no local
    // identity yet and nothing to key under.
    const routed = remoteMachineId(getState(), machineId);
    const key = machineId ?? getState().hello?.machineId;
    if (!key) return;
    // Guard against out-of-order commits: two overlapping refreshes for the
    // same machine can resolve in either order, so tag each with a generation
    // and only commit if a newer request hasn't started meanwhile.
    const gen = (archiveGen.get(key) ?? 0) + 1;
    archiveGen.set(key, gen);
    const { archives } = await client.request(
      "archive.list",
      routed ? { machineId: routed } : {},
    );
    if (archiveGen.get(key) !== gen) return;
    dispatch({ type: "archives", machineId: key, archives });
  };

  const refreshUpdate = async () => {
    // Cached read: the server polls master on its own schedule, so this never
    // waits on the network.
    const update = await client.request("git.selfUpdateStatus", {});
    dispatch({ type: "update", update });
  };

  // Navigate to a thread using an explicit route rather than reading it back
  // out of state. restoreArchive relies on this because a remote restore's
  // thread list may not be committed to React state yet when it navigates.
  const selectThreadRouted = async (
    threadId: string,
    machineId?: string,
    preserveFeed = false,
  ) => {
    // Reconnect/resync reads must not blank a transcript that is already on
    // screen. `openThread` intentionally clears the feed for real navigation;
    // doing that for a background refresh also resets every piece of mounted
    // UI state (open agent HUD, scroll position, expanded cards) and produces
    // a visible full-pane blink.
    if (!preserveFeed) {
      dispatch({ type: "openThread", threadId });
      localStorage.setItem(lastThreadKey(getState().solo), threadId);
    }
    try {
      const { thread, events } = await client.request(
        "thread.get",
        machineId ? { threadId, machineId } : { threadId },
      );
      if (!isAgentVisible(thread.agent)) {
        if (!preserveFeed) {
          localStorage.removeItem(lastThreadKey(getState().solo));
          dispatch({ type: "closeActive" });
        }
        return;
      }
      dispatch({ type: "feedLoaded", threadId, thread, events });
      // Repo summaries power the per-repo badges on chat diff cards — load
      // them once per project without waiting for the Git tab to be opened
      // (auto-routed to the owner for remote projects).
      if (!getState().git[thread.projectId]) {
        void refreshGitRepos(thread.projectId).catch(() => undefined);
      }
    } catch (err) {
      // An in-place refresh is opportunistic. Keep the transcript the user is
      // reading if its owner is still reconnecting; a later peer/open-socket
      // event will retry without replacing useful content with an error card.
      if (preserveFeed) return;
      const reason = err instanceof Error ? err.message : String(err);
      const known = findThread(getState(), threadId);
      // Only the owner saying "I don't have this" means the thread is really
      // gone. A remote thread whose machine is asleep fails here too, and
      // forgetting it — the old behaviour — closed the pane with no
      // explanation at all: the chat you clicked just didn't open.
      if (/unknown thread/i.test(reason) || !known) {
        localStorage.removeItem(lastThreadKey(getState().solo));
        dispatch({ type: "closeActive" });
        return;
      }
      dispatch({
        type: "feedLoaded",
        threadId,
        thread: known,
        events: [
          {
            seq: -1,
            ts: new Date().toISOString(),
            event: { kind: "error", message: `Can't open this chat — ${reason}.` },
          },
        ],
      });
    }
  };

  const selectThread = (
    threadId: string,
    options?: { preserveFeed?: boolean },
  ) => {
    const thread = findThread(getState(), threadId);
    if (thread && !isAgentVisible(thread.agent)) return Promise.resolve();
    return selectThreadRouted(threadId, routeFor(threadId), options?.preserveFeed);
  };

  const noteError = (threadId: string, message: string) => {
    dispatch({
      type: "agentEvent",
      threadId,
      seq: -1,
      timestamp: new Date().toISOString(),
      event: { kind: "error", message },
    });
  };

  return {
    refreshProjects,
    refreshWorkspaces,
    refreshPeers,
    refreshRemoteThreads,
    refreshThreads,
    selectThread,

    async toolOutput(threadId: string, callId: string) {
      const machineId = routeFor(threadId);
      const { output } = await client.request(
        "thread.toolOutput",
        machineId ? { threadId, callId, machineId } : { threadId, callId },
      );
      return output;
    },

    async addPeer(url: string, token?: string) {
      await client.request("peer.add", token ? { url, token } : { url });
      await refreshPeers();
    },

    async removePeer(machineId: string) {
      await client.request("peer.remove", { machineId });
      await refreshPeers();
    },

    async renameDevice(name: string) {
      await client.request("device.rename", { name });
      // friendlyName rides in hello — re-pull so Settings shows the change.
      const hello = await client.request("hello", {});
      dispatch({ type: "hello", data: hello });
    },

    async setDeviceAppearance(patch) {
      const data = await client.request("device.setAppearance", patch);
      const hello = getState().hello;
      if (hello) {
        dispatch({ type: "hello", data: { ...hello, avatar: data.avatar, color: data.color } });
      }
    },

    async setPeerAppearance(machineId, patch) {
      const peer = await client.request("peer.setAppearance", { machineId, ...patch });
      const s = getState();
      dispatch({
        type: "peers",
        peers: s.peers.map((p) => (p.machineId === peer.machineId ? peer : p)),
        discovered: s.discovered,
      });
    },

    async setPeerProfile(machineId, patch) {
      // Routed real edits: they run on the target machine (which updates its
      // own device.json and gossips the change). The name goes via
      // device.rename, avatar/color via device.setAppearance. The updated
      // peer record arrives back through the "peers" broadcast the gossip
      // triggers, so no local dispatch is needed here.
      if (patch.name !== undefined) {
        await client.request("device.rename", { machineId, name: patch.name });
      }
      if (patch.image !== undefined || patch.color !== undefined) {
        const appearance: { machineId: string; image?: string | null; color?: string | null } = {
          machineId,
        };
        if (patch.image !== undefined) appearance.image = patch.image;
        if (patch.color !== undefined) appearance.color = patch.color;
        await client.request("device.setAppearance", appearance);
      }
      await refreshPeers();
    },

    async renameWorkspace(workspaceId: string, name: string) {
      const ws = await client.request("workspace.rename", { workspaceId, name });
      dispatch({
        type: "workspaces",
        workspaces: getState().workspaces.map((w) => (w.id === ws.id ? ws : w)),
      });
    },

    async setWorkspaceFavorite(workspaceId: string, favorite: boolean) {
      const ws = await client.request("workspace.setFavorite", { workspaceId, favorite });
      dispatch({
        type: "workspaces",
        workspaces: getState().workspaces.map((w) => (w.id === ws.id ? ws : w)),
      });
    },

    async setWorkspaceHidden(workspaceId: string, hidden: boolean) {
      const ws = await client.request("workspace.setHidden", { workspaceId, hidden });
      dispatch({
        type: "workspaces",
        workspaces: getState().workspaces.map((w) => (w.id === ws.id ? ws : w)),
      });
    },

    async setWorkspaceImage(workspaceId: string, image?: string) {
      const ws = await client.request("workspace.setImage", {
        workspaceId,
        image: image ?? null,
      });
      dispatch({
        type: "workspaces",
        workspaces: getState().workspaces.map((w) => (w.id === ws.id ? ws : w)),
      });
    },

    // Browser logins belong to the machine whose Chrome holds the session, so
    // every one of these takes the machine it applies to; omitted means here.
    async listBrowserProfiles(machineId?: string) {
      const { profiles } = await client.request("browser.profile.list", { machineId });
      return profiles;
    },

    async createBrowserProfile(name: string, origins: string[], machineId?: string) {
      return client.request("browser.profile.create", { name, origins, machineId });
    },

    async updateBrowserProfile(
      profileId: string,
      patch: { name?: string; origins?: string[] },
      machineId?: string,
    ) {
      return client.request("browser.profile.update", { profileId, ...patch, machineId });
    },

    async deleteBrowserProfile(profileId: string, machineId?: string) {
      await client.request("browser.profile.delete", { profileId, machineId });
    },

    async listLibrary(machineId?: string) {
      return client.request("library.list", { machineId });
    },

    async installSkill(
      payload: {
        agents: import("./lib/protocol").SkillTarget[];
        catalogId?: string;
        source?: string;
        name?: string;
      },
      machineId?: string,
    ) {
      const { skill } = await client.request("library.skill.install", {
        ...payload,
        machineId,
      });
      return skill;
    },

    async copySkill(
      skillId: string,
      agents: import("./lib/protocol").SkillTarget[],
      machineId?: string,
    ) {
      await client.request("library.skill.copy", { skillId, agents, machineId });
    },

    async removeSkill(
      skillId: string,
      agents: import("./lib/protocol").SkillTarget[],
      machineId?: string,
    ) {
      await client.request("library.skill.remove", { skillId, agents, machineId });
    },

    async saveMcpServer(
      server: import("./lib/protocol").McpServerInfo,
      machineId?: string,
    ) {
      const saved = await client.request("library.mcp.save", { server, machineId });
      return saved.server;
    },

    async installMcpServer(
      payload: { catalogId: string; inputs?: Record<string, string>; name?: string },
      machineId?: string,
    ) {
      const saved = await client.request("library.mcp.install", { ...payload, machineId });
      return saved.server;
    },

    async deleteMcpServer(serverId: string, machineId?: string) {
      await client.request("library.mcp.delete", { serverId, machineId });
    },

    async listMobileDevices() {
      const { devices } = await client.request("mobile.device.list", {});
      return devices;
    },

    async beginMobilePairing() {
      return await client.request("mobile.pair.begin", {});
    },

    async cancelMobilePairing() {
      await client.request("mobile.pair.cancel", {});
    },

    async revokeMobileDevice(deviceId: string) {
      await client.request("mobile.device.revoke", { deviceId });
    },

    listThemes,

    async saveTheme(theme) {
      const saved = await client.request("theme.save", { theme });
      // The server also broadcasts state.changed("themes"); refresh now so the
      // caller's own client updates without waiting on the round trip.
      void listThemes().catch(() => undefined);
      return saved;
    },

    async removeTheme(themeId: string) {
      await client.request("theme.remove", { themeId });
      void listThemes().catch(() => undefined);
    },

    async aiPalette(imageDataUrl, hint) {
      // The desktop always designs on its own machine, so no machineId here.
      return client.request("theme.aiPalette", { imageDataUrl, hint });
    },

    async listHermesAgents() {
      const { agents } = await client.request("hermes.agent.list", {});
      // Keep the global copy in step: sidebar rows and chat headers render
      // agent avatars from it.
      dispatch({ type: "hermesAgents", agents });
      return agents;
    },

    async addHermesAgent(baseUrl: string, apiKey: string, name?: string) {
      return client.request("hermes.agent.add", { baseUrl, apiKey, name });
    },

    async removeHermesAgent(agentId: string) {
      await client.request("hermes.agent.remove", { agentId });
    },

    async setHermesAgentImage(agentId: string, image?: string) {
      return client.request("hermes.agent.setImage", { agentId, image: image ?? null });
    },

    async setHermesAgentAvatar(agentId: string, image: string | null) {
      const agent = await client.request("hermes.agent.setAvatar", { agentId, image });
      const s = getState();
      dispatch({
        type: "hermesAgents",
        agents: s.hermesAgents.map((a) => (a.id === agent.id ? agent : a)),
      });
      return agent;
    },

    async hermesAgentDetails(agentId: string) {
      return client.request("hermes.agent.details", { agentId });
    },

    async listClaudexProfiles() {
      const { profiles } = await client.request("claudex.profile.list", {});
      return profiles;
    },

    async addClaudexProfile(input: ClaudexProfileInput) {
      return client.request("claudex.profile.add", input);
    },

    async updateClaudexProfile(profileId: string, input: ClaudexProfileInput) {
      return client.request("claudex.profile.update", { ...input, profileId });
    },

    async removeClaudexProfile(profileId: string) {
      await client.request("claudex.profile.remove", { profileId });
    },

    async setClaudexProfileAvatar(profileId: string, image: string | null) {
      return client.request("claudex.profile.setAvatar", { profileId, image });
    },

    async testClaudexProfile(profileId: string) {
      return client.request("claudex.profile.test", { profileId });
    },

    openDraft(projectId: string, machineId?: string) {
      dispatch({
        type: "openDraft",
        draft: defaultDraft(getState(), projectId, machineId),
      });
    },

    openHermesDraft(hermesAgentId: string) {
      const s = getState();
      const model = s.hello?.agents
        .find((a) => a.id === "hermes")
        ?.models.find((m) => m.id === hermesAgentId);
      dispatch({
        type: "openDraft",
        draft: {
          projectId: HERMES_HOME_PROJECT_ID,
          machineId: s.hello?.machineId ?? "",
          agent: "hermes",
          settings: {
            model: hermesAgentId,
            effort: effortForModel(model),
            access: "full",
            mode: "build",
          },
        },
      });
    },

    async send(text: string, attachments?: OutgoingAttachment[]) {
      const s = getState();
      const atts = attachments && attachments.length > 0 ? attachments : undefined;
      if (s.draft) {
        rememberNewThreadSettings(s.draft.projectId, s.draft.agent, s.draft.settings);
        const draftRoute = remoteMachineId(s, s.draft.machineId);
        const thread = await client.request("thread.create", {
          projectId: s.draft.projectId,
          agent: s.draft.agent,
          settings: s.draft.settings,
          ...(draftRoute ? { machineId: draftRoute } : {}),
        });
        dispatch({ type: "threadUpserted", thread });
        dispatch({ type: "openThread", threadId: thread.id });
        dispatch({ type: "feedLoaded", threadId: thread.id, thread, events: [] });
        localStorage.setItem(lastThreadKey(s.solo), thread.id);
        await client.request("turn.start", {
          threadId: thread.id,
          text,
          attachments: atts,
          ...(draftRoute ? { machineId: draftRoute } : {}),
        });
      } else if (s.activeThreadId) {
        try {
          const machineId = routeFor(s.activeThreadId);
          await client.request("turn.start", {
            threadId: s.activeThreadId,
            text,
            attachments: atts,
            ...(machineId ? { machineId } : {}),
          });
        } catch (e) {
          noteError(s.activeThreadId, e instanceof Error ? e.message : String(e));
          throw e;
        }
      }
    },

    async steer(text: string) {
      const s = getState();
      if (!s.activeThreadId) return;
      try {
        const machineId = routeFor(s.activeThreadId);
        await client.request("turn.steer", {
          threadId: s.activeThreadId,
          text,
          ...(machineId ? { machineId } : {}),
        });
      } catch (e) {
        noteError(s.activeThreadId, e instanceof Error ? e.message : String(e));
        throw e;
      }
    },

    async interrupt() {
      const s = getState();
      if (!s.activeThreadId) return;
      const machineId = routeFor(s.activeThreadId);
      await client.request("turn.interrupt", {
        threadId: s.activeThreadId,
        ...(machineId ? { machineId } : {}),
      });
    },

    async respondApproval(approvalId: string, optionId: string) {
      const s = getState();
      if (!s.activeThreadId) return;
      dispatch({ type: "approvalPending", approvalId });
      try {
        const machineId = routeFor(s.activeThreadId);
        await client.request("approval.respond", {
          threadId: s.activeThreadId,
          approvalId,
          optionId,
          ...(machineId ? { machineId } : {}),
        });
      } catch (e) {
        noteError(s.activeThreadId, e instanceof Error ? e.message : String(e));
      }
    },

    setQuestionAnswers(requestId: string, answers: Record<string, string[]>) {
      dispatch({ type: "questionAnswer", requestId, answers });
    },

    async respondQuestion(requestId: string, answers: Record<string, string[]>) {
      const s = getState();
      if (!s.activeThreadId) return;
      dispatch({ type: "questionPending", requestId });
      try {
        const machineId = routeFor(s.activeThreadId);
        await client.request("question.respond", {
          threadId: s.activeThreadId,
          requestId,
          answers,
          ...(machineId ? { machineId } : {}),
        });
      } catch (e) {
        noteError(s.activeThreadId, e instanceof Error ? e.message : String(e));
      }
    },

    async setSettings(settings: ThreadSettings) {
      const s = getState();
      if (s.draft) {
        dispatch({ type: "draftSettings", agent: s.draft.agent, settings });
        rememberNewThreadSettings(s.draft.projectId, s.draft.agent, settings);
        return;
      }
      if (!s.activeThreadId) return;
      const cur = findThread(s, s.activeThreadId);
      if (cur) dispatch({ type: "threadUpserted", thread: { ...cur, settings } });
      try {
        const machineId = routeFor(s.activeThreadId);
        const thread = await client.request("thread.setSettings", {
          threadId: s.activeThreadId,
          settings,
          ...(machineId ? { machineId } : {}),
        });
        dispatch({ type: "threadUpserted", thread });
        rememberNewThreadSettings(thread.projectId, thread.agent, thread.settings);
      } catch {
        if (cur) dispatch({ type: "threadUpserted", thread: cur }); // roll back
      }
    },

    setDraftAgent(agent: Agent) {
      const s = getState();
      if (!s.draft || !s.hello) return;
      const info = s.hello.agents.find((a) => a.id === agent);
      const modelId = info?.defaultModel ?? info?.models[0]?.id ?? "";
      const model = info?.models.find((m) => m.id === modelId);
      const settings = {
        ...s.draft.settings,
        model: modelId,
        effort: effortForModel(model, undefined, agent === "claude"),
        wideContext: undefined,
        claudeChrome: undefined,
      };
      dispatch({
        type: "draftSettings",
        agent,
        settings,
      });
      rememberNewThreadSettings(s.draft.projectId, agent, settings);
    },

    async setThreadAgent(agent: Agent) {
      const s = getState();
      if (!s.activeThreadId || !s.hello) return;
      const cur = findThread(s, s.activeThreadId);
      if (!cur || cur.agent === agent) return;
      const info = s.hello.agents.find((a) => a.id === agent);
      const modelId = info?.defaultModel ?? info?.models[0]?.id ?? "";
      const model = info?.models.find((m) => m.id === modelId);
      const settings = {
        ...cur.settings,
        model: modelId,
        effort: effortForModel(model, undefined, agent === "claude"),
        wideContext: undefined,
        claudeChrome: undefined,
      };
      dispatch({ type: "threadUpserted", thread: { ...cur, agent, settings } });
      try {
        const machineId = routeFor(s.activeThreadId);
        const thread = await client.request("thread.setAgent", {
          threadId: s.activeThreadId,
          agent,
          settings,
          ...(machineId ? { machineId } : {}),
        });
        dispatch({ type: "threadUpserted", thread });
        rememberNewThreadSettings(thread.projectId, thread.agent, thread.settings);
      } catch (e) {
        dispatch({ type: "threadUpserted", thread: cur }); // roll back
        noteError(s.activeThreadId, e instanceof Error ? e.message : String(e));
      }
    },

    async reviewThread(
      agent: Agent,
      settings: { model?: string; effort?: string; access?: Access },
      instructions?: string,
    ) {
      const s = getState();
      if (!s.activeThreadId || !s.hello) return;
      const info = s.hello.agents.find((a) => a.id === agent);
      // Profile-backed kinds (hermes, claudex) have no default — their "models"
      // are registry entries, so the server refuses a review without one.
      const model = settings.model ?? info?.defaultModel ?? info?.models[0]?.id;
      try {
        const machineId = routeFor(s.activeThreadId);
        await client.request("thread.review", {
          threadId: s.activeThreadId,
          agent,
          ...(model ? { model } : {}),
          ...(settings.effort ? { effort: settings.effort } : {}),
          ...(settings.access ? { access: settings.access } : {}),
          ...(instructions ? { instructions } : {}),
          ...(machineId ? { machineId } : {}),
        });
        // The reviewer lane and the running turn both arrive over the
        // thread.list broadcast + event stream; nothing to patch optimistically.
      } catch (e) {
        noteError(s.activeThreadId, e instanceof Error ? e.message : String(e));
        throw e;
      }
    },

    async startParley(
      reviewers: {
        agent: Agent;
        model?: string;
        effort?: string;
        access?: Access;
        name?: string;
        personaId?: string;
        personality?: string;
      }[],
      opts: { rounds?: number; execute?: boolean; instructions?: string },
    ) {
      const s = getState();
      if (!s.activeThreadId || !s.hello) return;
      // Fill in each reviewer's default model — required for the profile-backed
      // kinds (hermes, claudex), whose "models" are registry entries.
      const specs = reviewers.map((r) => {
        const info = s.hello!.agents.find((a) => a.id === r.agent);
        const model = r.model ?? info?.defaultModel ?? info?.models[0]?.id;
        return {
          agent: r.agent,
          ...(model ? { model } : {}),
          ...(r.effort ? { effort: r.effort } : {}),
          ...(r.access ? { access: r.access } : {}),
          ...(r.name ? { name: r.name } : {}),
          ...(r.personaId ? { personaId: r.personaId } : {}),
          ...(r.personality ? { personality: r.personality } : {}),
        };
      });
      try {
        const machineId = routeFor(s.activeThreadId);
        await client.request("thread.parley.start", {
          threadId: s.activeThreadId,
          reviewers: specs,
          ...(opts.rounds ? { rounds: opts.rounds } : {}),
          ...(opts.execute ? { execute: true } : {}),
          ...(opts.instructions ? { instructions: opts.instructions } : {}),
          ...(machineId ? { machineId } : {}),
        });
        // Lanes, parley state and the running turns all arrive over the
        // thread.list broadcast + event stream; nothing to patch optimistically.
      } catch (e) {
        noteError(s.activeThreadId, e instanceof Error ? e.message : String(e));
        throw e;
      }
    },

    async savePersona(persona: ReviewerPersona) {
      await client.request("persona.save", { persona });
      // The identity broadcast refetches hello; refresh eagerly so the dialog
      // sees the save without waiting for the round trip.
      try {
        const hello = await client.request("hello", {});
        dispatch({ type: "hello", data: hello });
      } catch {
        /* the broadcast will land it */
      }
    },

    async deletePersona(personaId: string) {
      await client.request("persona.delete", { personaId });
      try {
        const hello = await client.request("hello", {});
        dispatch({ type: "hello", data: hello });
      } catch {
        /* the broadcast will land it */
      }
    },

    async addProject(path: string) {
      const project = await client.request("project.create", { path });
      await refreshProjects();
      dispatch({ type: "openDraft", draft: defaultDraft(getState(), project.id) });
    },

    async deleteProject(projectId: string) {
      await client.request("project.delete", { projectId }).catch(() => undefined);
      forgetNewThreadSettings(projectId);
      dispatch({ type: "projectDeleted", projectId });
    },

    async deleteThread(threadId: string, projectId: string) {
      const machineId = routeFor(threadId);
      await client
        .request("thread.delete", { threadId, ...(machineId ? { machineId } : {}) })
        .catch(() => undefined);
      const key = lastThreadKey(getState().solo);
      if (localStorage.getItem(key) === threadId) {
        localStorage.removeItem(key);
      }
      dispatch({ type: "threadDeleted", threadId, projectId });
    },

    async renameThread(threadId: string, title: string) {
      const machineId = routeFor(threadId);
      const thread = await client.request("thread.rename", {
        threadId,
        title,
        ...(machineId ? { machineId } : {}),
      });
      dispatch({ type: "threadUpserted", thread });
    },

    async setThreadSettled(threadId: string, settled: boolean) {
      const machineId = routeFor(threadId);
      let thread;
      try {
        thread = await client.request("thread.setSettled", {
          threadId,
          settled,
          ...(machineId ? { machineId } : {}),
        });
      } catch (err) {
        // The row's `canSettle` guard covers the steady state, but a turn can
        // start between the render and the click, and the server rejects a
        // settle on a busy thread. Say so — the alternative is a button that
        // silently does nothing.
        const id = noticeSeq++;
        dispatch({
          type: "noticeAdd",
          notice: {
            id,
            threadId,
            title: settled ? "Couldn't settle that chat" : "Couldn't bring that chat back",
            body: err instanceof Error ? err.message : String(err),
          },
        });
        setTimeout(() => dispatch({ type: "noticeDismiss", id }), 8000);
        return;
      }
      // Only after the server agrees: parking is an acknowledgement, so the
      // unread marker goes too (it outranks settled and would otherwise pull
      // the row straight back out). Clearing it BEFORE the request would
      // throw the marker away for good if the request then failed.
      if (settled) dispatch({ type: "attentionClear", threadId });
      dispatch({ type: "threadUpserted", thread });
      // Settling the chat you are READING must move you on: the open chat is
      // exempt from the shelf (threadSettled rule 0), so without this the row
      // sat where it was looking un-settled — until you clicked another chat
      // and the old one "vanished" into the collapsed shelf. Land on the next
      // live chat in the same project (sidebar order: newest-created first),
      // or on a fresh draft when the shelf just swallowed the last one.
      const s = getState();
      if (settled && s.activeThreadId === threadId) {
        const ctx = { attention: s.attention, activeThreadId: null };
        const autoSettleDays = getSidebarPrefs().autoSettleDays;
        const now = Date.now();
        const next = (s.threads[thread.projectId] ?? [])
          // `s` predates the upsert above, so exclude the just-settled chat
          // by id rather than trusting its settledAt to have landed.
          .filter(
            (t) => t.id !== threadId && !threadSettled(ctx, t, autoSettleDays, now),
          )
          .sort(
            (a, b) =>
              (a.createdAt < b.createdAt ? 1 : a.createdAt > b.createdAt ? -1 : 0) ||
              (a.id < b.id ? 1 : a.id > b.id ? -1 : 0),
          )[0];
        if (next) await selectThread(next.id);
        else dispatch({ type: "openDraft", draft: defaultDraft(s, thread.projectId) });
      }
    },

    async setThreadFavorite(threadId: string, favorite: boolean) {
      const machineId = routeFor(threadId);
      const thread = await client.request("thread.setFavorite", {
        threadId,
        favorite,
        ...(machineId ? { machineId } : {}),
      });
      dispatch({ type: "threadUpserted", thread });
    },

    threadPreview(threadId: string) {
      // Routed by the thread's owning machine, exactly like rename/delete: the
      // backend forwards the request to the peer when machineId names one.
      const machineId = routeFor(threadId);
      return client.request("thread.preview", {
        threadId,
        ...(machineId ? { machineId } : {}),
      });
    },

    async searchThreads(query: string, threadIds: string[]) {
      // One request per owning Threadknot keeps full transcripts on their machines
      // while still making the fleet-wide sidebar search feel like one index.
      const grouped = new Map<string | undefined, string[]>();
      for (const threadId of threadIds) {
        const machineId = routeFor(threadId);
        const group = grouped.get(machineId) ?? [];
        group.push(threadId);
        grouped.set(machineId, group);
      }
      const results = await Promise.all(
        [...grouped].map(async ([machineId, ids]) => {
          try {
            const result = await client.request("thread.search", {
              query,
              threadIds: ids,
              ...(machineId ? { machineId } : {}),
            });
            return result.threadIds;
          } catch {
            // One offline/outdated peer should not erase matches returned by
            // this machine or the rest of the fleet.
            return [];
          }
        }),
      );
      return results.flat();
    },

    listDir(path?: string, machineId?: string) {
      return client.request("fs.listDir", {
        ...(path ? { path } : {}),
        ...(machineId ? { machineId } : {}),
      });
    },

    fsTree(projectId: string, machineId?: string) {
      return client.request("fs.tree", {
        projectId,
        ...route(machineId ?? projectRoute(projectId)),
      });
    },

    fsRead(projectId: string, path: string, machineId?: string) {
      return client.request("fs.read", {
        projectId,
        path,
        ...route(machineId ?? projectRoute(projectId)),
      });
    },

    async attachRoot(workspaceId: string, machineId: string, path: string) {
      const { project } = await client.request("workspace.attachRoot", {
        workspaceId,
        machineId,
        path,
      });
      await refreshProjects();
      return project;
    },

    async detachRoot(workspaceId: string, machineId: string, projectId: string) {
      await client.request("workspace.detachRoot", { workspaceId, machineId, projectId });
      await refreshProjects();
    },

    async listArtifacts(projectId: string) {
      const { artifacts } = await client.request("artifacts.list", {
        projectId,
        ...route(projectRoute(projectId)),
      });
      dispatch({ type: "artifacts", projectId, artifacts });
      return artifacts;
    },

    async deleteArtifact(artifactId: string) {
      // Server broadcasts state.changed("artifacts") so every client refreshes.
      const s = getState();
      let owner: string | undefined;
      for (const pid of Object.keys(s.artifacts)) {
        if (s.artifacts[pid].some((a) => a.id === artifactId)) {
          owner = projectRoute(pid);
          break;
        }
      }
      await client.request("artifacts.delete", { artifactId, ...route(owner) });
    },

    scanPorts() {
      return client.request("ports.scan", {});
    },

    async startDictation() {
      const { recordingId } = await client.request("dictation.start", {});
      return recordingId;
    },

    async stopDictation(recordingId: string) {
      const { text } = await client.request("dictation.stop", { recordingId });
      return text;
    },

    async cancelDictation(recordingId: string) {
      await client.request("dictation.cancel", { recordingId });
    },

    refreshGitRepos,

    gitStatus(repoId: string) {
      return client.request("git.status", { repoId, ...route(repoRoute(repoId)) });
    },

    gitDiff(repoId: string, path: string, scope: "staged" | "worktree" | "untracked") {
      return client.request("git.diff", { repoId, path, scope, ...route(repoRoute(repoId)) });
    },

    gitStage(repoId: string, paths: string[]) {
      return client.request("git.stage", { repoId, paths, ...route(repoRoute(repoId)) });
    },

    gitUnstage(repoId: string, paths: string[]) {
      return client.request("git.unstage", { repoId, paths, ...route(repoRoute(repoId)) });
    },

    gitDiscard(repoId: string, paths: string[]) {
      return client.request("git.discard", { repoId, paths, ...route(repoRoute(repoId)) });
    },

    gitCommit(repoId: string, message: string) {
      return client.request("git.commit", { repoId, message, ...route(repoRoute(repoId)) });
    },

    gitBranches(repoId: string) {
      return client.request("git.branches", { repoId, ...route(repoRoute(repoId)) });
    },

    gitCheckout(repoId: string, branch: string, create?: boolean) {
      return client.request("git.checkout", {
        repoId,
        branch,
        ...(create ? { create } : {}),
        ...route(repoRoute(repoId)),
      });
    },

    gitPush(repoId: string) {
      return client.request("git.push", { repoId, ...route(repoRoute(repoId)) });
    },

    gitPull(repoId: string) {
      return client.request("git.pull", { repoId, ...route(repoRoute(repoId)) });
    },

    gitCommitMany(payload) {
      return client.request("git.commitMany", {
        ...payload,
        ...route(projectRoute(payload.projectId)),
      });
    },

    gitCheckoutMany(projectId: string, repoIds: string[], branch: string) {
      return client.request("git.checkoutMany", {
        projectId,
        repoIds,
        branch,
        ...route(projectRoute(projectId)),
      });
    },

    gitPr(repoId: string, title?: string, body?: string) {
      return client.request("git.pr", {
        repoId,
        ...(title ? { title, body } : {}),
        ...route(repoRoute(repoId)),
      });
    },

    async refreshTerminals(projectId: string) {
      const owner = route(projectRoute(projectId));
      const { terms } = await client.request("term.list", { projectId, ...owner });
      dispatch({ type: "terminals", projectId, terminals: terms });
    },

    async createTerminal(projectId: string, name?: string) {
      const owner = route(projectRoute(projectId));
      const term = await client.request("term.create", {
        projectId,
        ...(name ? { name } : {}),
        ...owner,
      });
      const { terms } = await client.request("term.list", { projectId, ...owner });
      dispatch({ type: "terminals", projectId, terminals: terms });
      return term;
    },

    async renameTerminal(termId: string, name: string) {
      // Server broadcasts state.changed("terminals") → refreshTerminals runs.
      await client.request("term.rename", { termId, name, ...route(termRoute(termId)) });
    },

    async deleteTerminal(termId: string) {
      // Server broadcasts state.changed("terminals") → refreshTerminals runs.
      await client
        .request("term.delete", { termId, ...route(termRoute(termId)) })
        .catch(() => undefined);
    },

    async refreshUsage() {
      await client.request("usage.refresh", {}).catch(() => undefined);
    },

    async fetchChangelog() {
      const { entries, notes } = await client.request("app.changelog", {});
      return { entries: entries ?? [], notes: notes ?? [] };
    },

    async refreshSchedules() {
      const { schedules } = await client.request("schedule.list", {});
      dispatch({ type: "schedules", schedules });
    },

    async createSchedule(payload) {
      const schedule = await client.request("schedule.create", payload);
      dispatch({ type: "schedules", schedules: [schedule, ...getState().schedules] });
      return schedule;
    },

    async updateSchedule(payload) {
      const schedule = await client.request("schedule.update", payload);
      dispatch({
        type: "schedules",
        schedules: getState().schedules.map((s) => (s.id === schedule.id ? schedule : s)),
      });
      return schedule;
    },

    async deleteSchedule(scheduleId) {
      await client.request("schedule.delete", { scheduleId }).catch(() => undefined);
      dispatch({
        type: "schedules",
        schedules: getState().schedules.filter((s) => s.id !== scheduleId),
      });
    },

    async runSchedule(scheduleId) {
      const { threadId } = await client.request("schedule.run", { scheduleId });
      return threadId;
    },

    async archiveThread(threadId: string, projectId: string) {
      const machineId = routeFor(threadId);
      await client
        .request("thread.archive", { threadId, ...(machineId ? { machineId } : {}) })
        .catch(() => undefined);
      const lastKey = lastThreadKey(getState().solo);
      if (localStorage.getItem(lastKey) === threadId) {
        localStorage.removeItem(lastKey);
      }
      dispatch({ type: "threadDeleted", threadId, projectId });
      // The archive now lives on the thread's owning machine — refresh that
      // machine's list (machineId is already the routed owner, or undefined).
      void refreshArchives(machineId);
    },

    refreshArchives,

    async restoreArchive(archiveId: string, machineId?: string) {
      const routed = remoteMachineId(getState(), machineId);
      const { thread } = await client.request(
        "archive.restore",
        routed ? { archiveId, machineId: routed } : { archiveId },
      );
      // Refresh the sidebar lists so the restored thread shows up in place.
      // refreshProjects covers local projects; the explicit refreshThreads
      // (routed) covers a remote project the fleet view doesn't hold locally.
      await refreshProjects();
      await refreshThreads(thread.projectId, routed);
      void refreshArchives(machineId);
      // Navigate with the known route, not routeFor: the refreshed thread list
      // above may not be committed to React state yet, and a remote restore's
      // thread.get must go to the owning machine or navigation fails.
      await selectThreadRouted(thread.id, routed);
    },

    async deleteArchive(archiveId: string, machineId?: string) {
      const routed = remoteMachineId(getState(), machineId);
      // Let the delete reject so the row can surface offline/permission errors.
      await client.request(
        "archive.delete",
        routed ? { archiveId, machineId: routed } : { archiveId },
      );
      // A refresh hiccup must not mask a delete that already succeeded.
      await refreshArchives(machineId).catch(() => undefined);
    },

    async getArchiveDir() {
      const { archiveDir } = await client.request("settings.get", {});
      return archiveDir;
    },

    async setArchiveDir(path: string) {
      const { archiveDir } = await client.request("settings.set", { archiveDir: path });
      return archiveDir;
    },

    refreshUpdate,

    async checkForUpdate() {
      // Returns immediately; the server fetches in the background and
      // broadcasts the `updates` scope when it has an answer. A synchronous
      // fetch can outlast the client's request timeout.
      await client.request("git.selfUpdateCheck", {}).catch(() => undefined);
    },

    async updateStatusFor(machineId?: string) {
      return client.request(
        "git.selfUpdateStatus",
        machineId ? { machineId } : {},
      );
    },

    async pullUpdate(machineId?: string) {
      await client.request("git.selfUpdatePull", machineId ? { machineId } : {});
      if (!machineId) void refreshUpdate().catch(() => undefined);
    },

    async rebuildUpdate(machineId?: string) {
      // Resolves when the build is claimed, not when it finishes: a release
      // compile takes minutes and the request timeout is 30s. Progress and the
      // final verdict arrive on the `updates` broadcast.
      await client.request("git.selfUpdateRebuild", machineId ? { machineId } : {});
      if (!machineId) void refreshUpdate().catch(() => undefined);
    },

    async restartUpdate(machineId?: string, force?: boolean) {
      await client.request("git.selfUpdateRestart", {
        ...(machineId ? { machineId } : {}),
        ...(force ? { force } : {}),
      });
    },

    async setUpdateRepoPath(path: string, machineId?: string) {
      await client.request("git.selfUpdateSetRepoPath", {
        path,
        ...(machineId ? { machineId } : {}),
      });
      if (!machineId) void refreshUpdate().catch(() => undefined);
    },
  };
}

/**
 * Coalesce repeated refreshes of the same key into one trailing call.
 *
 * A running turn nudges "threads changed" at every status transition — dozens
 * of times per turn, and more with several agents working at once. Each nudge
 * is cheap on its own, but firing them all keeps the socket busy competing
 * with the replay the user is waiting on. One refresh per burst says the same
 * thing.
 */
function coalesced(delayMs: number): (key: string, run: () => void) => void {
  const timers = new Map<string, ReturnType<typeof setTimeout>>();
  return (key, run) => {
    const existing = timers.get(key);
    if (existing) clearTimeout(existing);
    timers.set(
      key,
      setTimeout(() => {
        timers.delete(key);
        run();
      }, delayMs),
    );
  };
}

/** What (if anything) an incoming event should announce. */
function noticeBody(frame: EventFrame): string | null {
  if (frame.seq < 0) return null; // deltas / local-only errors
  switch (frame.event.kind) {
    case "turn_completed":
      return "Done";
    case "error":
      return "Failed";
    case "approval_request":
      return "Approval needed";
    case "question_request":
      return "Question waiting";
    default:
      return null;
  }
}

let noticeSeq = 1;

/**
 * Traycer-style suppression: viewing the thread with the window focused →
 * silence; every other state → system notification on top of the toast/chime.
 * A focused Threadknot window may be showing a different project or chat, which
 * still needs an OS-level alert rather than an easy-to-miss in-app toast.
 */
function maybeNotify(
  frame: EventFrame,
  state: AppState,
  dispatch: React.Dispatch<Action>,
  actions: ThreadknotActions,
) {
  const body = noticeBody(frame);
  if (!body) return;
  const prefs = getNotifyPrefs();
  if (!prefs.enabled) return;
  const evThread = findThread(state, frame.threadId);
  if (evThread && !isAgentVisible(evThread.agent)) return;
  // Per-client workspace subscriptions: two people sharing one Threadknot each
  // narrow this to their own work, and neither hears the other's agents.
  if (!wantsWorkspace(prefs, workspaceIdForProject(state, evThread?.projectId))) return;
  // Solo window: only announce its own project. Fleet window: stay quiet
  // about projects that have a dedicated solo window watching them.
  if (state.solo && evThread?.projectId !== state.solo) return;
  if (!state.solo && evThread && hasSoloWindow(evThread.projectId)) return;
  const focused = isWindowFocused();
  const viewing = state.activeThreadId === frame.threadId;
  if (state.isTauri) {
    void import("@tauri-apps/api/core")
      .then(({ invoke }) =>
        invoke("debug_note", {
          msg: `maybeNotify focused=${focused} viewing=${viewing} hasFocus=${document.hasFocus()} hidden=${document.hidden}`,
        }),
      )
      .catch(() => undefined);
  }
  if (focused && viewing) return;

  const thread = findThread(state, frame.threadId);
  const title = thread?.title?.trim() || "Threadknot";
  // Inside the mobile shell the server pushes the same moment through Expo —
  // chime/vibrate/system-notify here would double up. Keep only the toast.
  const native = isNativeShell();
  if (prefs.sound && !native) chime();
  if (!native) vibrate();
  if (!native) {
    void showSystemNotification(title, body, {
      isTauri: state.isTauri,
      onClick: () => void actions.selectThread(frame.threadId),
    });
  }
  const id = noticeSeq++;
  dispatch({ type: "noticeAdd", notice: { id, threadId: frame.threadId, title, body } });
  setTimeout(() => dispatch({ type: "noticeDismiss", id }), 8000);
}

export default function App() {
  const [state, dispatch] = useReducer(reducer, initialState);
  const [showPicker, setShowPicker] = useState(false);
  const [showSchedules, setShowSchedules] = useState(false);

  const stateRef = useRef(state);
  stateRef.current = state;

  const client = useMemo(() => new ThreadknotClient(), []);
  const actions = useMemo(
    () => makeActions(client, dispatch, () => stateRef.current),
    [client],
  );
  const actionsRef = useRef(actions);
  actionsRef.current = actions;

  // Unread completion markers survive a refresh and stay synchronized between
  // Threadknot's fleet and solo windows. They remain local to this browser/device.
  useEffect(() => persistThreadAttention(state.attention), [state.attention]);
  useEffect(() => {
    const onStorage = (event: StorageEvent) => {
      if (event.key === LS_THREAD_ATTENTION) {
        dispatch({ type: "attentionSync", attention: loadThreadAttention() });
      }
      // Notification prefs are cached for a stable React snapshot; another
      // window (a solo window, another tab) may have just rewritten them.
      if (event.key?.startsWith("threadknot.notify") || event.key === "threadknot.soundOff") {
        invalidateNotifyPrefs();
      }
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, []);

  useEffect(() => {
    let cancelled = false;
    let stopFocusTracking: (() => void) | undefined;
    const coalesce = coalesced(200);

    client.onStatus = (conn) => dispatch({ type: "conn", conn });
    client.onEvent = (frame) => {
      maybeNotify(frame, stateRef.current, dispatch, actionsRef.current);
      dispatch({
        type: "agentEvent",
        threadId: frame.threadId,
        seq: frame.seq,
        timestamp: frame.ts ?? new Date().toISOString(),
        speaker: frame.speaker,
        event: frame.event,
      });
      // An agent turn likely touched the tree — refresh the fleet view if the
      // project's Git tab is open (cheap: only fires at turn boundaries).
      if (frame.event.kind === "turn_completed" && frame.seq >= 0) {
        const t = findThread(stateRef.current, frame.threadId);
        if (t && stateRef.current.workspace[t.projectId] === "git") {
          void actionsRef.current.refreshGitRepos(t.projectId).catch(() => undefined);
        }
      }
    };
    client.onUsage = (frame) => dispatch({ type: "usage", usage: frame.usage });
    client.onHermesStatuses = (frame) =>
      dispatch({ type: "hermesStatuses", revision: frame.revision, statuses: frame.statuses });
    client.onStateChanged = (frame) => {
      if (frame.scope === "agents") {
        // Agent registry changed (Hermes add/remove) — re-pull the picker data.
        void client
          .request("hello", {})
          .then((hello) => dispatch({ type: "hello", data: hello }))
          .catch(() => undefined);
        void actionsRef.current.listHermesAgents().catch(() => undefined);
        return;
      }
      if (frame.scope === "schedules") void actionsRef.current.refreshSchedules();
      else if (frame.scope === "themes")
        void actionsRef.current.listThemes().catch(() => undefined);
      else if (frame.scope === "workspaces") void actionsRef.current.refreshWorkspaces();
      else if (frame.scope === "peers") {
        // A relayed `peers` frame describes the OTHER machine's own peer graph,
        // not a change to ours. Peers can flap on a short cadence, and treating
        // those frames as local used to reopen whichever remote chat happened
        // to be on screen every few seconds.
        if (frame.origin) return;
        void actionsRef.current.refreshPeers();
        // A peer just came or went — refill whatever that machine owns, and
        // refresh an open remote transcript in place. Never navigate to the
        // same chat again: that clears the feed and unmounts transient UI.
        coalesce("remoteThreads", () => {
          void actionsRef.current.refreshRemoteThreads().catch(() => undefined);
          const s = stateRef.current;
          const open = s.activeThreadId;
          const projectId = open ? findThread(s, open)?.projectId : undefined;
          if (open && projectId && remoteMachineId(s, projectOwner(s, projectId))) {
            void actionsRef.current
              .selectThread(open, { preserveFeed: true })
              .catch(() => undefined);
          }
        });
      }
      else if (frame.scope === "identity") {
        // A routed profile edit changed THIS machine's own device.json; re-pull
        // hello so the sidebar reflects the new avatar/name/color.
        void client
          .request("hello", {})
          .then((hello) => dispatch({ type: "hello", data: hello }))
          .catch(() => undefined);
      }
      else if (frame.scope === "updates")
        void actionsRef.current.refreshUpdate().catch(() => undefined);
      else if (frame.scope === "archives")
        // A relayed peer broadcast carries `origin` (the peer's machineId);
        // refresh THAT machine's list. A locally produced frame has no origin,
        // so this refreshes the local machine's archives.
        void actionsRef.current.refreshArchives(frame.origin);
      else if (frame.scope === "projects") void actionsRef.current.refreshProjects();
      else if (frame.scope === "terminals" && frame.projectId)
        void actionsRef.current.refreshTerminals(frame.projectId);
      else if (frame.scope === "artifacts" && frame.projectId)
        void actionsRef.current.listArtifacts(frame.projectId);
      else if (frame.scope === "git" && frame.projectId)
        void actionsRef.current.refreshGitRepos(frame.projectId);
      else if (frame.projectId) {
        // Relayed frames from a peer carry ITS project ids — resolve the
        // owner from workspace membership so the refresh routes back to it.
        const projectId = frame.projectId;
        coalesce(`threads:${projectId}`, () => {
          const s = stateRef.current;
          const owner = remoteMachineId(s, projectOwner(s, projectId));
          void actionsRef.current.refreshThreads(projectId, owner).catch(() => undefined);
        });
      } else {
        coalesce("projects", () => void actionsRef.current.refreshProjects());
      }
    };
    client.onOpen = (isReconnect) => {
      void (async () => {
        try {
          const hello = await client.request("hello", {});
          dispatch({ type: "hello", data: hello });
          postToNative({
            type: "ready",
            serverId: hello.serverId,
            serverName: hello.serverName,
          });
          void client
            .request("usage.get", {})
            .then(({ usage }) => dispatch({ type: "usage", usage }))
            .catch(() => undefined);
          await actionsRef.current.refreshProjects();
          void actionsRef.current.refreshSchedules().catch(() => undefined);
          void actionsRef.current.refreshArchives().catch(() => undefined);
          void actionsRef.current.refreshUpdate().catch(() => undefined);
          void actionsRef.current.refreshPeers().catch(() => undefined);
          void actionsRef.current.listHermesAgents().catch(() => undefined);
          // Seed the machine's custom themes; the `themes` state-changed
          // broadcast keeps them fresh from here on.
          void actionsRef.current.listThemes().catch(() => undefined);
          // Seed live Hermes presence; the `hermes.statuses` broadcast keeps it
          // fresh from here on.
          void client
            .request("hermes.agent.statuses", {})
            .then(({ revision, statuses }) =>
              dispatch({ type: "hermesStatuses", revision, statuses }),
            )
            .catch(() => undefined);

          const openId = stateRef.current.activeThreadId;
          if (isReconnect && openId) {
            // Replay the full transcript without dispatching `openThread`: the
            // same chat is still selected, so blanking it would only flash the
            // pane and close mounted popovers while the socket catches up.
            await actionsRef.current.selectThread(openId, { preserveFeed: true });
          } else if (!isReconnect) {
            // Restore last-open thread; selectThread self-heals if it's gone.
            const solo = stateRef.current.solo;
            const last = localStorage.getItem(lastThreadKey(solo));
            if (last) await actionsRef.current.selectThread(last);
            else if (solo) {
              // Fresh solo window: land on the project's most recent thread.
              const recent = (stateRef.current.threads[solo] ?? [])[0];
              if (recent) await actionsRef.current.selectThread(recent.id);
            }
          }
        } catch {
          // server will broadcast state.changed when it settles
        }
      })();
    };

    // Mobile shell: install the bridge entry point and honor push-tap
    // navigation (queued until this handler exists, so cold starts work).
    initNativeBridge();
    setNativeNavigationHandler((nav) => {
      void actionsRef.current.selectThread(nav.threadId);
    });
    // A suspended mobile WebView may wake with a dead transport that never
    // delivered `close`. Renewing it runs the ordinary reconnect resync, which
    // refreshes the selected transcript without navigating away or blanking it.
    setNativeResumeHandler(() => client.reconnect());
    let stopMobileResumeTracking: (() => void) | undefined;
    if (isNativeShell()) {
      // This path comes from the server-served web bundle, so installed mobile
      // shells benefit after a page reload even before the next native build.
      const onPageResume = () => {
        if (!document.hidden) client.reconnect();
      };
      document.addEventListener("visibilitychange", onPageResume);
      window.addEventListener("pageshow", onPageResume);
      stopMobileResumeTracking = () => {
        document.removeEventListener("visibilitychange", onPageResume);
        window.removeEventListener("pageshow", onPageResume);
      };
    }

    let stopSoloAdvert: (() => void) | undefined;
    void (async () => {
      try {
        // Resolve solo mode BEFORE connecting so restore/notify logic sees it.
        const solo = await detectSoloProject().catch(() => null);
        if (cancelled) return;
        if (solo) {
          dispatch({ type: "solo", projectId: solo });
          stopSoloAdvert = advertiseSoloWindow(solo);
        }
        const target = await discoverServer();
        if (cancelled) return;
        dispatch({ type: "isTauri", value: target.isTauri });
        stopFocusTracking = startFocusTracking(target.isTauri);
        dispatch({ type: "http", value: { base: target.httpBase, token: target.token } });
        client.connect(target.wsUrl);
      } catch {
        if (!cancelled) dispatch({ type: "conn", conn: "offline" });
      }
    })();

    return () => {
      cancelled = true;
      setNativeNavigationHandler(null);
      setNativeResumeHandler(null);
      stopMobileResumeTracking?.();
      stopFocusTracking?.();
      stopSoloAdvert?.();
      client.dispose();
    };
  }, [client]);

  // Ctrl/cmd + wheel and ctrl/cmd + = / - / 0 drive the conversation zoom
  // (terminals handle ctrl+wheel themselves for their font size). No pane
  // measurement here: the zoom scales the message feed only, so it has no
  // ceiling beyond ZOOM_MAX and nothing to observe.
  useEffect(() => initZoomHotkeys(), []);

  useEffect(() => installExternalLinkHandler(), []);

  // Keep the mobile shell informed of where we are and whether we're online.
  useEffect(() => {
    if (!isNativeShell()) return;
    const thread = state.activeThreadId ? findThread(state, state.activeThreadId) : undefined;
    postToNative({
      type: "routeChanged",
      projectId: thread?.projectId,
      threadId: state.activeThreadId ?? undefined,
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state.activeThreadId]);

  useEffect(() => {
    if (!isNativeShell()) return;
    postToNative({ type: "connectionChanged", conn: state.conn });
  }, [state.conn]);

  // Solo browser tabs name themselves after the project (Tauri windows get
  // their title from the Rust side at creation).
  useEffect(() => {
    if (!state.solo || state.isTauri) return;
    const p = state.projects.find((x) => x.id === state.solo);
    if (p) document.title = `${p.name} — Threadknot`;
  }, [state.solo, state.isTauri, state.projects]);

  const store = useMemo(() => ({ state, dispatch, actions }), [state, actions]);

  async function onAddProject() {
    if (state.isTauri) {
      try {
        const dir = await pickDirectoryNative();
        if (dir) await actions.addProject(dir);
      } catch (e) {
        console.error("add project failed", e);
      }
    } else {
      setShowPicker(true);
    }
  }

  return (
    <StoreContext.Provider value={store}>
      <div className="app">
        {/* Custom themes live in server state, so boot-time apply has to wait
            for the records to arrive; this headless bridge re-applies the
            active one whenever the store's theme list or the choice changes. */}
        <ThemeSync />
        <Sidebar
          onAddProject={() => void onAddProject()}
          onOpenSchedules={() => setShowSchedules(true)}
        />
        {state.sidebarOpen && (
          <div
            className="sidebar-backdrop"
            onClick={() => dispatch({ type: "sidebar", open: false })}
          />
        )}
        <div className="work-pane">
          <MainSplit />
        </div>
        {state.conn !== "online" && (
          <div className={`conn-banner conn-${state.conn}`}>
            {state.conn === "connecting" ? "connecting to server…" : "offline — retrying…"}
          </div>
        )}
        {state.notices.length > 0 && (
          <div className="toast-stack">
            {state.notices.map((n) => (
              <button
                key={n.id}
                type="button"
                className="toast"
                onClick={() => {
                  dispatch({ type: "noticeDismiss", id: n.id });
                  void actions.selectThread(n.threadId);
                }}
              >
                <span className="toast-title">{n.title}</span>
                <span className="toast-body">{n.body}</span>
              </button>
            ))}
          </div>
        )}
        {!state.solo && state.dragProject && (
          <div
            className="popout-zone"
            onDragOver={(e) => {
              e.preventDefault();
              e.dataTransfer.dropEffect = "copy";
            }}
            onDrop={(e) => {
              e.preventDefault();
              const p = state.dragProject;
              dispatch({ type: "dragProject", project: null });
              if (p) void openProjectWindow(p);
            }}
          >
            <div className="popout-zone-inner">
              <span className="popout-zone-title">{state.dragProject.name}</span>
              <span>drop to open in its own window</span>
            </div>
          </div>
        )}
        {showPicker && <DirPicker onClose={() => setShowPicker(false)} />}
        {showSchedules && <SchedulesPanel onClose={() => setShowSchedules(false)} />}
        <AvatarCropHost />
        {!state.isTauri && <PullToRefresh />}
      </div>
    </StoreContext.Provider>
  );
}

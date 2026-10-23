// Which Chrome tab is linked? Chrome's own "debugging this browser" bar shows in EVERY tab, so it
// can't say. A linked tab is put in a tab group per agent, "maiTerm · <agent tab>", and put back
// where it was when the link ends, however it ends (docs/browser-link.md §1.8).
//
// A link carries its `placement`: the window, the tab that was just before it (its index as a
// fallback), and the group it went into. Restoring only touches a tab still in that group: one the
// human has dragged elsewhere since is left where they put it. A tab already in a group of the
// human's own, a pinned tab, and a tab in a popup window are never grouped.
//
// Chrome groups on its own too: a tab opened from a grouped tab joins that group. A tab that joins
// its opener's agent is then ALREADY in our group (`joined`: ungrouped at the end, never moved), and
// one that doesn't join is taken straight back out (`releaseIfOurs`), or the group would show the
// agent a tab that isn't its.

const COLORS = ['blue', 'purple', 'cyan', 'green', 'orange', 'pink', 'yellow', 'red'];
const PREFIX = 'maiTerm · ';
const NONE = -1; // chrome.tabGroups.TAB_GROUP_ID_NONE

/** The same agent gets the same colour every time. */
function colorFor(maitermTabId) {
  let h = 0;
  for (const c of maitermTabId) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return COLORS[h % COLORS.length];
}

function titleFor(link) {
  return `${PREFIX}${link.maitermTabName ?? 'agent'}`;
}

/** The agent's live group in this window, from any of its other links. */
async function agentGroupIn(link, links, windowId) {
  const ids = new Set(
    links
      .filter((l) => l !== link && l.host === link.host && l.maitermTabId === link.maitermTabId && l.placement?.groupId != null)
      .map((l) => l.placement.groupId),
  );
  for (const id of ids) {
    const g = await chrome.tabGroups.get(id).catch(() => null);
    if (g && g.windowId === windowId) return g.id;
  }
  return undefined;
}

/** Every group one of our links put a tab in. */
function ourGroups(links) {
  return new Set(links.map((l) => l.placement?.groupId).filter((id) => id != null));
}

/**
 * Group a newly linked tab with its agent's other tabs in that window. Returns its placement, or
 * undefined when it isn't grouped. `carried`: the placement it had before (relinked, or moving
 * from another agent), which counts only while the tab is still in that group — once the human
 * has moved it, where they put it is its place now.
 */
export async function mark(link, links, carried) {
  try {
    const tab = await chrome.tabs.get(link.chromeTabId);
    if (tab.pinned) return undefined;
    const win = await chrome.windows.get(tab.windowId);
    if (win.type !== 'normal') return undefined;
    const agentGroup = await agentGroupIn(link, links, tab.windowId);

    const here = async () => {
      const before = tab.index > 0 ? (await chrome.tabs.query({ windowId: tab.windowId, index: tab.index - 1 }))[0] : undefined;
      return { windowId: tab.windowId, index: tab.index, afterTabId: before?.id ?? null };
    };
    let placement;
    if (carried && tab.groupId === carried.groupId) {
      // A joined tab was born in its opener's group and never had another place; moving it to
      // another agent's group moves it, so where it is now becomes the place to put it back.
      placement = carried.joined && carried.groupId !== agentGroup ? await here() : carried;
    } else if (tab.groupId !== NONE && tab.groupId === agentGroup) {
      // Chrome put it there itself (opened from one of the agent's tabs): it was never anywhere else.
      return { windowId: tab.windowId, groupId: agentGroup, joined: true };
    } else if (tab.groupId !== NONE) {
      return undefined; // the human's own group
    } else {
      placement = await here();
    }

    let groupId = agentGroup;
    if (groupId != null) {
      if (tab.groupId !== groupId) await retry(() => chrome.tabs.group({ groupId, tabIds: [tab.id] }));
    } else {
      groupId = await retry(() => chrome.tabs.group({ tabIds: [tab.id], createProperties: { windowId: tab.windowId } }));
      await chrome.tabGroups.update(groupId, { title: titleFor(link), color: colorFor(link.maitermTabId) });
    }
    return { ...placement, groupId };
  } catch {
    // A tab that closed meanwhile, or a browser without tab groups: no marker, the link stands.
    return undefined;
  }
}

/** Chrome refuses tab edits while the human is dragging a tab ("Tabs cannot be edited right
 *  now"): wait and try again, or a link ending mid-drag would leave its tab in the group. */
async function retry(fn) {
  for (let i = 0; ; i++) {
    try {
      return await fn();
    } catch (e) {
      if (i >= 10 || !/cannot be edited/i.test(String(e?.message ?? e))) throw e;
      await new Promise((r) => setTimeout(r, 300));
    }
  }
}

/** Out of its group; Chrome pulls a tab moved between two members of a group back into it. */
async function ungroupFully(tabId) {
  await retry(() => chrome.tabs.ungroup(tabId));
  const t = await chrome.tabs.get(tabId);
  if (t.groupId !== NONE) await retry(() => chrome.tabs.ungroup(tabId));
}

/** The link ended: out of the group, and back after the tab it followed (else its old index). */
export async function unmark(link) {
  const p = link.placement;
  if (!p) return;
  try {
    const tab = await chrome.tabs.get(link.chromeTabId);
    if (tab.groupId !== p.groupId) return; // the human moved it; leave it where they put it
    await retry(() => chrome.tabs.ungroup(tab.id));
    if (p.joined || tab.windowId !== p.windowId) return;
    const after = p.afterTabId != null ? await chrome.tabs.get(p.afterTabId).catch(() => null) : null;
    let index;
    if (p.afterTabId == null) index = 0;
    else if (after && after.windowId === p.windowId) {
      const now = await chrome.tabs.get(tab.id);
      index = after.index < now.index ? after.index + 1 : after.index;
    } else index = p.index;
    await retry(() => chrome.tabs.move(tab.id, { index }));
    // Landed between two of the agent's remaining tabs: Chrome regroups it, so take it out again.
    const moved = await chrome.tabs.get(tab.id);
    if (moved.groupId !== NONE) await ungroupFully(tab.id);
  } catch {
    // Closed: nothing to put back.
  }
}

/** A tab opened from one of our tabs that did NOT join its agent: Chrome grouped it with the
 *  opener, which would show it as the agent's. Take it out. */
export async function releaseIfOurs(tab, links) {
  try {
    const t = await chrome.tabs.get(tab.id);
    if (t.groupId !== NONE && ourGroups(links).has(t.groupId)) await ungroupFully(t.id);
  } catch {
    // Closed already.
  }
}

/** Groups no link owns: the extension's storage was cleared (reloaded, updated, the browser
 *  restarted) while Chrome kept the groups. Their places are lost; they are at least ungrouped. */
export async function sweepOrphans(links) {
  try {
    const owned = ourGroups(links);
    for (const g of await chrome.tabGroups.query({})) {
      if (!g.title?.startsWith(PREFIX) || owned.has(g.id)) continue;
      const tabs = await chrome.tabs.query({ groupId: g.id });
      if (tabs.length) await chrome.tabs.ungroup(tabs.map((t) => t.id));
    }
  } catch {
    // No tab groups here.
  }
}

/** Its agent tab was renamed or reloaded: the group's title follows. */
export async function retitle(link) {
  const id = link.placement?.groupId;
  if (id == null) return;
  await chrome.tabGroups.update(id, { title: titleFor(link) }).catch(() => {});
}

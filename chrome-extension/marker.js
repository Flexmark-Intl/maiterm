// Which Chrome tab is linked? Chrome's own "debugging this browser" bar shows in EVERY tab, so it
// can't say. A linked tab is put in a tab group per agent, "maiTerm · <agent tab>", and put back
// where it was when the link ends, however it ends (docs/browser-link.md §1.8).
//
// A link carries its `placement`: the window, the tab that was just before it (its index as a
// fallback), and the group it went into. Restoring only touches a tab still in that group: one the
// human has dragged elsewhere since is left where they put it. A tab already in a group of the
// human's own, a pinned tab, and a tab in a popup window are never grouped.

const COLORS = ['blue', 'purple', 'cyan', 'green', 'orange', 'pink', 'yellow', 'red'];

/** The same agent gets the same colour every time. */
function colorFor(maitermTabId) {
  let h = 0;
  for (const c of maitermTabId) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return COLORS[h % COLORS.length];
}

function titleFor(link) {
  return `maiTerm · ${link.maitermTabName ?? 'agent'}`;
}

/**
 * Group a newly linked tab with its agent's other tabs in that window. Returns its placement, or
 * undefined when it isn't grouped. `carried`: the placement it had under the agent it is moving
 * from, so restoring still returns it to where the HUMAN had it.
 */
export async function mark(link, links, carried) {
  try {
    const tab = await chrome.tabs.get(link.chromeTabId);
    if (tab.pinned) return undefined;
    const ours = carried && tab.groupId === carried.groupId;
    if (tab.groupId !== chrome.tabGroups.TAB_GROUP_ID_NONE && !ours) return undefined;
    const win = await chrome.windows.get(tab.windowId);
    if (win.type !== 'normal') return undefined;

    let placement = carried;
    if (!placement) {
      const before = tab.index > 0 ? (await chrome.tabs.query({ windowId: tab.windowId, index: tab.index - 1 }))[0] : undefined;
      placement = { windowId: tab.windowId, index: tab.index, afterTabId: before?.id ?? null };
    }
    // The agent's group in this window, if it still has one.
    const sibling = links.find(
      (l) => l !== link && l.host === link.host && l.maitermTabId === link.maitermTabId && l.placement?.groupId != null,
    );
    let groupId;
    if (sibling) {
      const g = await chrome.tabGroups.get(sibling.placement.groupId).catch(() => null);
      if (g && g.windowId === tab.windowId) groupId = g.id;
    }
    if (groupId != null) {
      await chrome.tabs.group({ groupId, tabIds: [tab.id] });
    } else {
      groupId = await chrome.tabs.group({ tabIds: [tab.id], createProperties: { windowId: tab.windowId } });
      await chrome.tabGroups.update(groupId, { title: titleFor(link), color: colorFor(link.maitermTabId) });
    }
    return { ...placement, groupId };
  } catch {
    // A tab that closed meanwhile, or a browser without tab groups: no marker, the link stands.
    return undefined;
  }
}

/** The link ended: out of the group, and back after the tab it followed (else its old index). */
export async function unmark(link) {
  const p = link.placement;
  if (!p) return;
  try {
    const tab = await chrome.tabs.get(link.chromeTabId);
    if (tab.groupId !== p.groupId) return; // the human moved it; leave it where they put it
    await chrome.tabs.ungroup(tab.id);
    if (tab.windowId !== p.windowId) return;
    const after = p.afterTabId != null ? await chrome.tabs.get(p.afterTabId).catch(() => null) : null;
    let index;
    if (p.afterTabId == null) index = 0;
    else if (after && after.windowId === p.windowId) {
      const now = await chrome.tabs.get(tab.id);
      index = after.index < now.index ? after.index + 1 : after.index;
    } else index = p.index;
    await chrome.tabs.move(tab.id, { index });
  } catch {
    // Closed: nothing to put back.
  }
}

/** Its agent tab was renamed or reloaded: the group's title follows. */
export async function retitle(link) {
  const id = link.placement?.groupId;
  if (id == null) return;
  await chrome.tabGroups.update(id, { title: titleFor(link) }).catch(() => {});
}

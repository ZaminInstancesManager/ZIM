/**
 * `zim://newtab` — the ZIM new-tab page.
 *
 * Engine health, whoami, the visible server count, sign-in when
 * needed, and quick links. No lifecycle side effects on load (the
 * session-restoration rule, BROWSER_INTEGRATION.md §6).
 */
import { health, listInstances, whoami } from "./api";
import "./components";
const statusPill = document.getElementById("engine-status");
const whoPill = document.getElementById("who");
const countPill = document.getElementById("server-count");
const loginCard = document.getElementById("login-card");
const consoleLink = document.getElementById("ql-console");
const LAST_CONSOLE_KEY = "zim.lastConsole";
async function refresh() {
    // Engine health — unauthenticated.
    try {
        await health();
        setPill(statusPill, "ok", "ok", "pill pill-ok");
    }
    catch {
        setPill(statusPill, "unreachable", "unreachable", "pill pill-unknown");
    }
    // Session + server list (one round trip each; 401 is expected).
    let me = null;
    try {
        me = await whoami();
    }
    catch {
        me = null;
    }
    if (me !== null) {
        setPill(whoPill, me.username, me.username, "pill pill-ok");
        try {
            const instances = await listInstances();
            setPill(countPill, String(instances.length), `${instances.length}`, "pill pill-ok");
        }
        catch {
            setPill(countPill, "—", "—", "pill pill-unknown");
        }
    }
    else {
        setPill(whoPill, "not signed in", "not signed in", "pill pill-unknown");
        setPill(countPill, "—", "—", "pill pill-unknown");
    }
    if (loginCard !== null) {
        loginCard.hidden = me !== null;
    }
    // Quick link: the last-visited console (session restoration is
    // view-only; this is a plain link, never a lifecycle call).
    const last = window.localStorage.getItem(LAST_CONSOLE_KEY);
    if (consoleLink !== null && last !== null && last !== "") {
        consoleLink.setAttribute("href", `/zim/servers/${encodeURIComponent(last)}/console`);
        consoleLink.hidden = false;
    }
}
function setPill(element, text, aria, className) {
    if (element === null) {
        return;
    }
    element.textContent = text;
    element.setAttribute("aria-label", aria);
    element.className = className;
}
document.addEventListener("zim:authenticated", () => {
    void refresh();
});
document.addEventListener("zim:signed-out", () => {
    void refresh();
});
void refresh();

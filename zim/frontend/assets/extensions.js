/**
 * `zim://extensions` — the add-on management page (M24).
 *
 * Lists every installed addon with its manifest contributions
 * (surfaces, sidebar items, commands, events, dependencies), its
 * permission split (requested vs. granted), its signature class,
 * and its §4.10 notice when the engine disabled it. Admins get the
 * lifecycle controls — enable/disable, grant, uninstall, and
 * upload — enforced engine-side; non-admins see the same inventory
 * read-only (§10.12's list is any-session).
 *
 * The themes section lists the engine-side uploaded theme packages
 * (M23's deferred upload surface, delivered here) with install and
 * remove for admins.
 *
 * View-and-control only for the registry: no addon page is loaded
 * here, and nothing on this page touches an instance.
 */
import { ApiError, deleteTheme, grantExtension, installExtension, listExtensions, listThemes, setExtensionEnabled, uninstallExtension, uploadTheme, whoami, } from "./api.js";
import "./components.js";
import { initThemeFromStorage } from "./theme.js";
// The theme applies before anything renders (M23: live, everywhere).
initThemeFromStorage();
const authGate = document.getElementById("auth-gate");
const stateEl = document.getElementById("ext-state");
const listEl = document.getElementById("ext-list");
const uploadLabel = document.getElementById("upload-label");
const uploadInput = document.getElementById("upload-input");
const bootNoticesCard = document.getElementById("boot-notices");
const bootNoticesList = document.getElementById("boot-notices-list");
const themeStateEl = document.getElementById("theme-state");
const themeListEl = document.getElementById("theme-list");
const themeUploadLabel = document.getElementById("theme-upload-label");
const themeUploadInput = document.getElementById("theme-upload-input");
/** Whether the signed-in user may manage addons (engine enforces
 * regardless; this only gates the affordances honestly). */
let isAdmin = false;
/** Render one addon's card. */
function renderAddon(addon) {
    const card = document.createElement("article");
    card.className = "card ext-card";
    if (!addon.enabled) {
        card.classList.add("ext-disabled");
    }
    // Header: name, id, version, badges.
    const head = document.createElement("div");
    head.className = "console-toolbar";
    const title = document.createElement("h3");
    title.className = "ext-name";
    title.textContent = addon.name;
    const idPill = document.createElement("span");
    idPill.className = "pill pill-muted mono ext-id";
    idPill.textContent = addon.id;
    const versionPill = document.createElement("span");
    versionPill.className = "pill pill-muted mono";
    versionPill.textContent = `v${addon.version}`;
    head.append(title, idPill, versionPill);
    const badges = document.createElement("span");
    badges.className = "ext-badges";
    if (addon.bundled) {
        const bundled = document.createElement("span");
        bundled.className = "pill";
        bundled.textContent = "bundled default";
        badges.append(bundled);
    }
    const signature = document.createElement("span");
    signature.className = "pill";
    signature.textContent = `signature: ${addon.signature}`;
    badges.append(signature);
    if (!addon.enabled) {
        const off = document.createElement("span");
        off.className = "pill pill-danger";
        off.textContent = "disabled";
        badges.append(off);
    }
    head.append(badges);
    card.append(head);
    if (addon.description !== "") {
        const description = document.createElement("p");
        description.className = "ext-description";
        description.textContent = addon.description;
        card.append(description);
    }
    if (addon.notice !== undefined && addon.notice !== null) {
        const notice = document.createElement("p");
        notice.className = "empty ext-notice";
        notice.textContent = `Engine notice: ${addon.notice}`;
        card.append(notice);
    }
    // The manifest inventory, as definition rows.
    const facts = document.createElement("dl");
    facts.className = "status-grid ext-facts";
    const fact = (term, detail) => {
        const dt = document.createElement("dt");
        dt.textContent = term;
        const dd = document.createElement("dd");
        dd.textContent = detail;
        facts.append(dt, dd);
    };
    fact("Instance scope", addon.instance_scope);
    fact("Permissions (requested)", addon.permissions.requested.join(", ") || "(none)");
    fact("Permissions (granted)", addon.permissions.granted.join(", ") || "(none)");
    fact("Page surfaces", addon.page_surfaces.join("\n") || "(none)");
    fact("Sidebar items", addon.sidebar_items.map((item) => item.label).join(", ") || "(none)");
    fact("Context menu", addon.context_menu_items.map((item) => item.label).join(", ") || "(none)");
    fact("Commands", addon.commands.map((command) => command.id + (command.accelerator !== undefined ? ` (${command.accelerator})` : "")).join(", ") || "(none)");
    fact("Event subscriptions", addon.event_subscriptions.join(", ") || "(none)");
    fact("Dependencies", addon.dependencies.join(", ") || "(none)");
    fact("Installed", new Date(addon.installed_at).toLocaleString());
    card.append(facts);
    // Admin controls.
    if (isAdmin) {
        const controls = document.createElement("div");
        controls.className = "console-toolbar ext-controls";
        const toggle = document.createElement("button");
        toggle.className = "btn";
        toggle.type = "button";
        toggle.textContent = addon.enabled ? "Disable" : "Enable";
        toggle.addEventListener("click", () => {
            void lifecycle(addon.id, !addon.enabled);
        });
        controls.append(toggle);
        if (!addon.bundled) {
            const remove = document.createElement("button");
            remove.className = "btn btn-danger";
            remove.type = "button";
            remove.textContent = "Uninstall";
            remove.addEventListener("click", () => {
                void uninstall(addon.id);
            });
            controls.append(remove);
        }
        // §4.2's elevation path: grant additional permissions by name.
        const grantForm = document.createElement("form");
        grantForm.className = "ext-grant";
        const grantInput = document.createElement("input");
        grantInput.className = "input mono";
        grantInput.type = "text";
        grantInput.placeholder = "grant: control.start, file.read…";
        grantInput.setAttribute("aria-label", `Additional permissions for ${addon.id}`);
        const grantButton = document.createElement("button");
        grantButton.className = "btn";
        grantButton.type = "submit";
        grantButton.textContent = "Grant";
        grantForm.append(grantInput, grantButton);
        grantForm.addEventListener("submit", (event) => {
            event.preventDefault();
            const permissions = grantInput.value
                .split(",")
                .map((name) => name.trim())
                .filter((name) => name !== "");
            if (permissions.length === 0) {
                return;
            }
            void grant(addon.id, permissions);
        });
        controls.append(grantForm);
        card.append(controls);
    }
    return card;
}
/** Render one uploaded theme's card. */
function renderTheme(theme) {
    const card = document.createElement("article");
    card.className = "card ext-card";
    const head = document.createElement("div");
    head.className = "console-toolbar";
    const title = document.createElement("h3");
    title.className = "ext-name";
    title.textContent = theme.name;
    const idPill = document.createElement("span");
    idPill.className = "pill pill-muted mono ext-id";
    idPill.textContent = theme.id;
    head.append(title, idPill);
    card.append(head);
    if (theme.description !== "") {
        const description = document.createElement("p");
        description.className = "ext-description";
        description.textContent = theme.description;
        card.append(description);
    }
    const tokens = document.createElement("p");
    tokens.className = "ext-tokens mono";
    tokens.textContent = theme.overrides.map((name) => `${name} = ${theme.tokens[name] ?? ""}`).join("  ·  ");
    card.append(tokens);
    if (isAdmin) {
        const controls = document.createElement("div");
        controls.className = "console-toolbar";
        const remove = document.createElement("button");
        remove.className = "btn btn-danger";
        remove.type = "button";
        remove.textContent = "Remove";
        remove.addEventListener("click", () => {
            void removeTheme(theme.id);
        });
        controls.append(remove);
        card.append(controls);
    }
    return card;
}
async function refresh() {
    try {
        const envelope = await listExtensions();
        const notices = envelope.notices ?? [];
        if (bootNoticesCard !== null && bootNoticesList !== null) {
            bootNoticesCard.hidden = notices.length === 0;
            bootNoticesList.replaceChildren(...notices.map((notice) => {
                const item = document.createElement("li");
                item.textContent = notice;
                return item;
            }));
        }
        stateEl?.classList.remove("empty-error");
        if (envelope.data.length === 0) {
            stateEl.textContent =
                "No add-ons installed. The bundled defaults appear on a fresh engine; upload a .zimpkg to install one.";
            listEl?.replaceChildren();
        }
        else {
            stateEl.textContent = "";
            listEl?.replaceChildren(...envelope.data.map(renderAddon));
        }
    }
    catch (error) {
        stateEl?.classList.add("empty-error");
        stateEl.textContent = describe(error);
    }
}
async function refreshThemes() {
    try {
        const envelope = await listThemes();
        if (envelope.data.length === 0) {
            themeStateEl.textContent = "No uploaded themes yet.";
            themeListEl?.replaceChildren();
        }
        else {
            themeStateEl.textContent = "";
            themeListEl?.replaceChildren(...envelope.data.map(renderTheme));
        }
    }
    catch (error) {
        themeStateEl.textContent = describe(error);
    }
}
async function lifecycle(id, enabled) {
    try {
        await setExtensionEnabled(id, enabled);
        await refresh();
    }
    catch (error) {
        stateEl?.classList.add("empty-error");
        stateEl.textContent = describe(error);
    }
}
async function uninstall(id) {
    try {
        await uninstallExtension(id);
        await refresh();
    }
    catch (error) {
        stateEl?.classList.add("empty-error");
        stateEl.textContent = describe(error);
    }
}
async function grant(id, permissions) {
    try {
        await grantExtension(id, permissions);
        await refresh();
    }
    catch (error) {
        stateEl?.classList.add("empty-error");
        stateEl.textContent = describe(error);
    }
}
async function removeTheme(id) {
    try {
        await deleteTheme(id);
        await refreshThemes();
    }
    catch (error) {
        themeStateEl.textContent = describe(error);
    }
}
/** Read an uploaded file as the raw bytes the engine expects. */
function readBytes(file) {
    return file.arrayBuffer();
}
async function installUploaded(file) {
    try {
        const bytes = await readBytes(file);
        const summary = await installExtension(bytes);
        stateEl?.classList.remove("empty-error");
        stateEl.textContent = `Installed ${summary.id} ${summary.version}.`;
        await refresh();
    }
    catch (error) {
        stateEl?.classList.add("empty-error");
        stateEl.textContent = describe(error);
    }
}
async function installUploadedTheme(file) {
    try {
        const bytes = await readBytes(file);
        const theme = await uploadTheme(bytes);
        themeStateEl.textContent = `Installed theme ${theme.name}.`;
        await refreshThemes();
    }
    catch (error) {
        themeStateEl.textContent = describe(error);
    }
}
/** One honest error line (the engine's §6 vocabulary). */
function describe(error) {
    if (error instanceof ApiError) {
        const missing = error.missingPermissions.length > 0
            ? ` (needs ${error.missingPermissions.join(", ")})`
            : "";
        return `${error.code}: ${error.message}${missing}`;
    }
    return String(error);
}
// ── Boot ──────────────────────────────────────────────────────────
async function boot() {
    const me = await whoami();
    if (me === null) {
        if (authGate !== null) {
            authGate.hidden = false;
        }
        stateEl.textContent = "Sign in to list the engine's add-ons.";
        themeStateEl.textContent = "Sign in to list uploaded themes.";
        return;
    }
    isAdmin = Boolean(me.is_admin || me.root_admin);
    if (isAdmin) {
        uploadLabel?.removeAttribute("hidden");
        themeUploadLabel?.removeAttribute("hidden");
    }
    await Promise.all([refresh(), refreshThemes()]);
}
uploadInput?.addEventListener("change", () => {
    const file = uploadInput.files?.[0];
    if (file !== undefined) {
        void installUploaded(file);
        uploadInput.value = "";
    }
});
themeUploadInput?.addEventListener("change", () => {
    const file = themeUploadInput.files?.[0];
    if (file !== undefined) {
        void installUploadedTheme(file);
        themeUploadInput.value = "";
    }
});
void boot();

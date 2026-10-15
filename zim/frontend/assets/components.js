/**
 * ZIM default frontend — Web Components.
 *
 * Vanilla custom elements, no framework. Styling inside shadow roots
 * uses constructable stylesheets (adoptedStyleSheets), which keeps
 * every rule in a same-origin stylesheet — the zim:// CSP forbids
 * inline `<style>` and `style=` attributes.
 */
import { ApiError, frontendActive, login, logout, whoami } from "./api.js";
/** Relay a `zim:*` happening to the embedding app chrome (M22), if
 * this page runs inside one. CustomEvents stop at the document
 * boundary — iframes are separate documents — so the same-origin
 * parent gets a structured postMessage instead. Targets this
 * origin only; a cross-origin parent gets nothing. */
function relayToParent(type, detail) {
    if (window.parent === window) {
        return;
    }
    try {
        window.parent.postMessage({ type, detail }, window.location.origin);
    }
    catch {
        // A cross-origin parent (embedding ZIM pages elsewhere) — drop.
    }
}
/** Shared constructable stylesheet for the shadow roots.
 *
 * Tokenized against the theme custom properties (M23): custom
 * properties inherit through shadow boundaries, so a live theme
 * recolors the components too. The `var(--…, fallback)` spellings
 * keep the components honest when embedded outside the app's
 * stylesheet (each fallback is the default theme's value). */
const shadowCss = new CSSStyleSheet();
shadowCss.replaceSync(`
  :host { display: block; }

  /* status dot */
  .dot {
    display: inline-block;
    width: 10px; height: 10px;
    border-radius: 999px;
    vertical-align: middle;
    margin-right: 6px;
    background: var(--color-indicator-unknown, #9ca3af);
    flex: none;
  }
  .dot-green  { background: var(--color-indicator-running, #22c55e); }
  .dot-yellow { background: var(--color-indicator-starting, #f59e0b); }
  .dot-red    { background: var(--color-indicator-crashed, #ef4444); }
  .dot-blue   { background: var(--color-indicator-stopped, #3b82f6); }
  .dot-gray   { background: var(--color-indicator-unknown, #9ca3af); }
  .dot-label {
    font-size: 12px;
    color: var(--color-text-secondary, #9aa1ac);
    vertical-align: middle;
  }
  .dot-crash::after {
    content: "!";
    color: var(--color-indicator-crashed, #ef4444);
    font-weight: 700;
    font-size: 11px;
    margin-left: 3px;
    vertical-align: middle;
  }
  /* The §4 transient states pulse (starting, stopping, installing,
   * restoring, transferring) — the tab-strip and page dots share
   * this animation. Chrome's throbber slot, repurposed. */
  .dot-pulse {
    animation: zim-dot-pulse 1.2s ease-in-out infinite;
  }
  @keyframes zim-dot-pulse {
    0%   { opacity: 1;   transform: scale(1); }
    50%  { opacity: 0.45; transform: scale(0.82); }
    100% { opacity: 1;   transform: scale(1); }
  }
  /* The §4 progress overlay: a yellow ring that fills as the
   * install/restore/transfer progresses (the "progress" attribute
   * maps onto --progress via the CSSOM — CSP forbids style
   * attributes in markup, not CSSOM writes). */
  .dot-progress {
    background: conic-gradient(var(--color-indicator-starting, #f59e0b) calc(var(--progress, 0) * 360deg), var(--color-surface-2, #24282f) 0);
  }
  /* The small tab-strip variant (favicon-slot sized). */
  :host([data-size="sm"]) .dot { width: 8px; height: 8px; margin-right: 0; }
  :host([data-size="sm"]) .dot-label { display: none; }
  :host([data-size="sm"]) .dot-crash::after { font-size: 9px; margin-left: 1px; }

  /* login form */
  form { display: flex; flex-direction: column; gap: 8px; margin: 0; }
  label { font-size: 11px; text-transform: uppercase; letter-spacing: 0.06em;
          color: var(--color-text-secondary, #9aa1ac); display: flex; flex-direction: column; gap: 4px; }
  input {
    padding: 8px 10px; border: 1px solid var(--color-surface-2, #24282f); border-radius: 4px;
    background: var(--color-background, #14161a); color: var(--color-text-primary, #e6e8ec); font: inherit; font-size: 13px;
  }
  input:focus-visible { outline: 2px solid var(--color-accent, #3b82f6); outline-offset: 1px; }
  button {
    align-self: flex-start;
    padding: 6px 14px; border-radius: 4px; cursor: pointer;
    border: 1px solid var(--color-accent, #3b82f6); background: var(--color-accent, #3b82f6); color: var(--color-background, #14161a);
    font: inherit; font-size: 13px;
  }
  button:disabled { opacity: 0.45; cursor: not-allowed; }
  .error { color: var(--color-danger, #ef4444); font-size: 12px; min-height: 1em; }
  .row { display: flex; align-items: center; gap: 8px; }
  .who { font-size: 12px; color: var(--color-text-secondary, #9aa1ac); }
  .linkish { background: none; border: none; color: var(--color-accent, #3b82f6); cursor: pointer;
             font: inherit; font-size: 12px; padding: 0; }
  .linkish:hover { text-decoration: underline; }

  /* console view */
  .console {
    display: block;
    background: var(--console-ink, #101216);
    border-radius: 6px;
    border: 1px solid var(--color-surface-2, #24282f);
    min-height: 320px;
    max-height: 60vh;
    overflow-y: auto;
    padding: 10px 12px;
    margin: 0;
    font-family: ui-monospace, "Cascadia Mono", Consolas, monospace;
    font-size: 12px;
    line-height: 1.45;
    white-space: pre-wrap;
    word-break: break-all;
  }
  .console:focus-visible { outline: 2px solid var(--color-accent, #3b82f6); }
  .line { display: block; }
  .line-dim { color: var(--color-text-secondary, #6b7280); }
  .line-err { color: var(--color-danger, #ef4444); }

  /* notices */
  .notice {
    border-radius: 6px; padding: 12px 16px; margin: 0;
    border: 1px solid var(--color-warning, #f59e0b); color: var(--color-warning, #f59e0b);
    background: color-mix(in srgb, var(--color-warning, #f59e0b) 8%, transparent);
    font-size: 13px;
  }
  .notice strong { display: block; margin-bottom: 2px; }
`);
/** Attach the shared stylesheet to a shadow root. */
function adopt(root) {
    // Cast for older DOM typings; runtime support is universal in the
    // Chromium builds ZIM targets.
    const withSheets = root;
    withSheets.adoptedStyleSheets = [...(withSheets.adoptedStyleSheets ?? []), shadowCss];
}
// ── <zim-status-dot> ──────────────────────────────────────────────
/**
 * The §4 status indicator: a colored dot plus label, driven by the
 * `status` attribute (an engine lifecycle status string). The
 * transient yellow states pulse; `progress` (0–1) adds the §4
 * progress ring for installing/restoring/transferring.
 */
export class ZimStatusDot extends HTMLElement {
    static get observedAttributes() {
        return ["status", "data-label", "progress", "data-size"];
    }
    constructor() {
        super();
        const root = this.attachShadow({ mode: "open" });
        adopt(root);
        const dot = document.createElement("span");
        dot.className = "dot";
        const label = document.createElement("span");
        label.className = "dot-label";
        root.append(dot, label);
    }
    attributeChangedCallback(name) {
        if (name === "progress") {
            this.applyProgress();
            return;
        }
        this.render();
    }
    connectedCallback() {
        this.render();
    }
    render() {
        const root = this.shadowRoot;
        if (root === null) {
            return;
        }
        const dot = root.querySelector(".dot");
        const label = root.querySelector(".dot-label");
        if (dot === null || label === null) {
            return;
        }
        const status = this.getAttribute("status") ?? "unknown";
        const color = visualClass(status);
        const transient = status === "starting" ||
            status === "stopping" ||
            status === "installing" ||
            status === "restoring_backup" ||
            status === "transferring";
        dot.className = `dot dot-${color}${transient ? " dot-pulse" : ""}`;
        if (status === "crash_loop") {
            dot.classList.add("dot-crash");
        }
        this.applyProgress();
        label.textContent = this.getAttribute("data-label") ?? status;
        this.setAttribute("role", "img");
        this.setAttribute("aria-label", `status: ${this.getAttribute("data-label") ?? status}`);
    }
    /** Map the `progress` attribute (0–1) onto the ring's custom
     * property; out-of-range values simply drop the ring. */
    applyProgress() {
        const dot = this.shadowRoot?.querySelector(".dot");
        if (!(dot instanceof HTMLElement)) {
            return;
        }
        const raw = this.getAttribute("progress");
        const value = raw !== null ? Number.parseFloat(raw) : Number.NaN;
        if (Number.isFinite(value) && value > 0 && value < 1) {
            dot.classList.add("dot-progress");
            dot.style.setProperty("--progress", String(value));
        }
        else {
            dot.classList.remove("dot-progress");
            dot.style.removeProperty("--progress");
        }
    }
}
/** The §4 color for a status string (the engine's canonical
 * `visual_color`, kept in lockstep with `api.ts` and
 * `domain::InstanceStatus`). */
function visualClass(status) {
    switch (status) {
        case "running":
            return "green";
        case "starting":
        case "stopping":
        case "installing":
        case "restoring_backup":
        case "transferring":
            return "yellow";
        case "crash_loop":
            return "red";
        case "offline":
            return "blue";
        case "suspended":
            return "gray";
        default:
            return "gray";
    }
}
// ── <zim-recovery-notice> ─────────────────────────────────────────
/**
 * The §3.6 recovery notice: when the engine reverted to the default
 * frontend (a broken custom package), `zim://newtab` must say so.
 * The engine flags this both in the URL (`?recovery=1` on the served
 * redirect) and in `GET /frontend/active`'s `recovery_notice`.
 */
export class ZimRecoveryNotice extends HTMLElement {
    constructor() {
        super();
        const root = this.attachShadow({ mode: "open" });
        adopt(root);
        const paragraph = document.createElement("p");
        paragraph.className = "notice";
        root.append(paragraph);
        this.style.display = "none";
    }
    connectedCallback() {
        void this.check();
    }
    async check() {
        const params = new URLSearchParams(window.location.search);
        const flagged = params.get("recovery") === "1";
        let reason = "";
        if (flagged) {
            reason = "the previous frontend package failed its activation check";
        }
        else {
            try {
                const active = await frontendActive();
                if (active && active.recovery_notice) {
                    reason = active.recovery_notice.reason;
                }
            }
            catch {
                // Unauthenticated or the engine is unreachable: the banner
                // stays hidden — the pages' own error handling covers that.
            }
        }
        if (!reason) {
            return;
        }
        const paragraph = this.shadowRoot?.querySelector(".notice") ?? null;
        if (paragraph === null) {
            return;
        }
        paragraph.textContent = "";
        const strong = document.createElement("strong");
        strong.textContent = "ZIM restored the default frontend";
        paragraph.append(strong);
        paragraph.append(document.createTextNode(`Reason: ${reason}. ` +
            "The engine kept the previous package on disk; an administrator can inspect it via the frontend endpoints."));
        this.style.display = "block";
    }
}
// ── <zim-login-form> ──────────────────────────────────────────────
/**
 * The email + password sign-in form. On success the engine sets the
 * session cookie and the element dispatches `zim:authenticated`
 * (bubbling) with the user payload; the hosting page then reloads
 * its data.
 */
export class ZimLoginForm extends HTMLElement {
    constructor() {
        super();
        this.form = null;
        this.emailInput = null;
        this.passwordInput = null;
        this.submitButton = null;
        this.errorText = null;
        this.signedInRow = null;
        this.whoText = null;
        this.signingOut = false;
        const root = this.attachShadow({ mode: "open" });
        adopt(root);
        root.append(this.buildSignedOut());
        root.append(this.buildSignedIn());
    }
    connectedCallback() {
        void this.refresh();
    }
    buildSignedOut() {
        const form = document.createElement("form");
        const emailLabel = document.createElement("label");
        emailLabel.textContent = "Email";
        const email = document.createElement("input");
        email.type = "email";
        email.name = "email";
        email.required = true;
        email.autocomplete = "username";
        email.placeholder = "admin@zim.local";
        emailLabel.append(email);
        const passwordLabel = document.createElement("label");
        passwordLabel.textContent = "Password";
        const password = document.createElement("input");
        password.type = "password";
        password.name = "password";
        password.required = true;
        password.autocomplete = "current-password";
        passwordLabel.append(password);
        const submit = document.createElement("button");
        submit.type = "submit";
        submit.textContent = "Sign in";
        const error = document.createElement("p");
        error.className = "error";
        error.setAttribute("role", "alert");
        form.append(emailLabel, passwordLabel, submit, error);
        form.addEventListener("submit", (event) => {
            event.preventDefault();
            void this.submit();
        });
        this.form = form;
        this.emailInput = email;
        this.passwordInput = password;
        this.submitButton = submit;
        this.errorText = error;
        return form;
    }
    buildSignedIn() {
        const row = document.createElement("div");
        row.className = "row";
        row.style.display = "none";
        const who = document.createElement("span");
        who.className = "who";
        const signOut = document.createElement("button");
        signOut.type = "button";
        signOut.className = "linkish";
        signOut.textContent = "Sign out";
        signOut.addEventListener("click", () => {
            void this.signOut();
        });
        row.append(who, signOut);
        this.signedInRow = row;
        this.whoText = who;
        return row;
    }
    async refresh() {
        const me = await whoami();
        this.applyUser(me);
    }
    applyUser(me) {
        const signedIn = me !== null && !this.signingOut;
        if (this.form !== null) {
            this.form.style.display = signedIn ? "none" : "flex";
        }
        if (this.signedInRow !== null) {
            this.signedInRow.style.display = signedIn ? "flex" : "none";
        }
        if (signedIn && this.whoText !== null && me !== null) {
            this.whoText.textContent = `${me.username} (${me.email})`;
        }
    }
    async submit() {
        const email = this.emailInput?.value.trim() ?? "";
        const password = this.passwordInput?.value ?? "";
        if (this.submitButton !== null) {
            this.submitButton.disabled = true;
        }
        if (this.errorText !== null) {
            this.errorText.textContent = "";
        }
        try {
            const me = await login(email, password);
            if (this.passwordInput !== null) {
                this.passwordInput.value = "";
            }
            this.applyUser(me);
            this.dispatchEvent(new CustomEvent("zim:authenticated", {
                bubbles: true,
                composed: true,
                detail: me,
            }));
            relayToParent("zim:authenticated", me);
        }
        catch (error) {
            const message = error instanceof ApiError ? error.message : String(error);
            if (this.errorText !== null) {
                this.errorText.textContent = message;
            }
        }
        finally {
            if (this.submitButton !== null) {
                this.submitButton.disabled = false;
            }
        }
    }
    async signOut() {
        this.signingOut = true;
        try {
            await logout();
        }
        catch {
            // A failed logout still clears locally — the cookie may be gone.
        }
        this.signingOut = false;
        this.applyUser(null);
        this.dispatchEvent(new CustomEvent("zim:signed-out", { bubbles: true, composed: true }));
        relayToParent("zim:signed-out");
    }
}
// ── <zim-console> ─────────────────────────────────────────────────
/**
 * The console output view: a capped, auto-scrolling log. When the
 * reader scrolls up, autoscroll pauses (a marker line resumes it on
 * the next append... simply: autoscroll resumes when the reader
 * scrolls back to the bottom).
 */
export class ZimConsole extends HTMLElement {
    static get observedAttributes() {
        return ["max-lines"];
    }
    constructor() {
        super();
        this.log = null;
        this.lineCount = 0;
        this.maxLines = 2000;
        this.follow = true;
        const root = this.attachShadow({ mode: "open" });
        adopt(root);
        const log = document.createElement("div");
        log.className = "console";
        log.tabIndex = 0;
        log.setAttribute("role", "log");
        log.setAttribute("aria-live", "polite");
        root.append(log);
        this.log = log;
        log.addEventListener("scroll", () => {
            if (log.scrollHeight - log.scrollTop - log.clientHeight < 24) {
                this.follow = true;
            }
            else {
                this.follow = false;
            }
        });
    }
    attributeChangedCallback(name, _old, value) {
        if (name === "max-lines" && value !== null) {
            const parsed = Number.parseInt(value, 10);
            if (Number.isFinite(parsed) && parsed > 0) {
                this.maxLines = parsed;
            }
        }
    }
    /** Append one console line (already text). */
    appendLine(line, kind = "out") {
        const log = this.log;
        if (log === null) {
            return;
        }
        const element = document.createElement("span");
        element.className = `line${kind === "err" ? " line-err" : kind === "dim" ? " line-dim" : ""}`;
        element.textContent = line.endsWith("\n") ? line.slice(0, -1) : line;
        if (element.textContent === "") {
            element.textContent = " ";
        }
        log.append(element);
        this.lineCount += 1;
        while (this.lineCount > this.maxLines && log.firstChild !== null) {
            log.firstChild.remove();
            this.lineCount -= 1;
        }
        if (this.follow) {
            log.scrollTop = log.scrollHeight;
        }
    }
    /** Replace the whole log (initial buffer load). */
    replaceAll(lines) {
        const log = this.log;
        if (log === null) {
            return;
        }
        while (log.firstChild !== null) {
            log.firstChild.remove();
        }
        this.lineCount = 0;
        for (const line of lines.slice(-this.maxLines)) {
            this.appendLine(line);
        }
    }
}
// ── registration ──────────────────────────────────────────────────
customElements.define("zim-status-dot", ZimStatusDot);
customElements.define("zim-recovery-notice", ZimRecoveryNotice);
customElements.define("zim-login-form", ZimLoginForm);
customElements.define("zim-console", ZimConsole);

/**
 * ZIM default frontend — live event stream client (§11).
 *
 * One WebSocket per subscribed instance, sub-protocol
 * `zim-events.v1`, reconnecting with exponential backoff and
 * replay-from-checkpoint (`?since_generation={n}`) per §11.3.
 * `stream.gap` events are surfaced so the page can re-fetch state
 * over HTTP instead of trusting a partial replay.
 */
/** Backoff schedule: 1 s, 2 s, 4 s, 8 s, capped at 15 s. */
const BACKOFF_FLOOR_MS = 1000;
const BACKOFF_CAP_MS = 15000;
/** How many consecutive failures before giving up entirely. */
const MAX_ATTEMPTS = 6;
/**
 * A self-healing §11 WebSocket subscription for one instance.
 *
 * The caller keeps exactly one per page (servers page keeps one per
 * subscribed row) and calls {@link ZimStream.close} on teardown.
 */
export class ZimStream {
    constructor(instanceId, handlers) {
        this.socket = null;
        this.lastGeneration = 0;
        this.attempts = 0;
        this.closed = false;
        this.instanceId = instanceId;
        this.handlers = handlers;
        this.connect();
    }
    /** The last event generation this client has seen. */
    get generation() {
        return this.lastGeneration;
    }
    /** Whether the client is currently connected. */
    get connected() {
        return this.socket !== null && this.socket.readyState === WebSocket.OPEN;
    }
    /** Permanently close the subscription (teardown). */
    close() {
        this.closed = true;
        if (this.reconnectTimer !== undefined) {
            clearTimeout(this.reconnectTimer);
            this.reconnectTimer = undefined;
        }
        if (this.socket !== null) {
            const socket = this.socket;
            this.socket = null;
            socket.close();
        }
        this.handlers.onStatus?.("down");
    }
    connect() {
        if (this.closed) {
            return;
        }
        this.handlers.onStatus?.(this.attempts === 0 ? "connecting" : "reconnecting");
        const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
        const query = new URLSearchParams({ instance_id: this.instanceId });
        if (this.lastGeneration > 0) {
            query.set("since_generation", String(this.lastGeneration));
        }
        const url = `${protocol}//${window.location.host}/api/v1/stream?${query.toString()}`;
        let socket;
        try {
            socket = new WebSocket(url, ["zim-events.v1"]);
        }
        catch (error) {
            this.handlers.onFatal?.(String(error));
            return;
        }
        this.socket = socket;
        socket.onopen = () => {
            this.attempts = 0;
            this.handlers.onStatus?.("live");
        };
        socket.onmessage = (message) => {
            if (typeof message.data !== "string") {
                return;
            }
            let event;
            try {
                event = JSON.parse(message.data);
            }
            catch {
                return; // A non-JSON frame is not part of the v1 contract.
            }
            if (typeof event.generation === "number" && event.generation > this.lastGeneration) {
                this.lastGeneration = event.generation;
                this.handlers.onGeneration?.(this.lastGeneration);
            }
            if (event.type === "stream.gap") {
                const from = typeof event.data.from === "number" ? event.data.from : 0;
                const to = typeof event.data.to === "number" ? event.data.to : 0;
                this.handlers.onGap?.(from, to);
            }
            this.handlers.onEvent(event);
        };
        socket.onclose = (event) => {
            if (this.closed) {
                return;
            }
            this.socket = null;
            // 1008 = policy violation: the upgrade was refused (auth or
            // permission). Retrying cannot fix either — surface and stop.
            if (event.code === 1008) {
                this.handlers.onFatal?.(event.reason || "upgrade rejected");
                this.handlers.onStatus?.("down");
                return;
            }
            this.scheduleReconnect();
        };
        socket.onerror = () => {
            // onclose follows; the reconnect decision happens there.
        };
    }
    scheduleReconnect() {
        this.attempts += 1;
        if (this.attempts > MAX_ATTEMPTS) {
            this.handlers.onStatus?.("down");
            return;
        }
        const delay = Math.min(BACKOFF_FLOOR_MS * 2 ** (this.attempts - 1), BACKOFF_CAP_MS);
        this.handlers.onStatus?.("reconnecting");
        this.reconnectTimer = setTimeout(() => {
            this.reconnectTimer = undefined;
            this.connect();
        }, delay);
    }
}

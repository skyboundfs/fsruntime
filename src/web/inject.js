(function () {
  "use strict";

  var SESSION = Number(window.__MSFS_SESSION__ || 0);
  var BASE = window.__MSFS_BASE__ || "";

  function request(params, body) {
    var url = BASE + "/api?session=" + SESSION + "&" + params;
    var xhr = new XMLHttpRequest();
    xhr.open(body === undefined ? "GET" : "POST", url, false);
    if (body !== undefined) {
      xhr.setRequestHeader("Content-Type", "text/plain");
    }
    try {
      xhr.send(body === undefined ? null : body);
    } catch (e) {
      return { error: String(e) };
    }
    try {
      return JSON.parse(xhr.responseText || "{}");
    } catch (e) {
      return { error: "bad bridge response" };
    }
  }

  function encode(value) {
    return encodeURIComponent(String(value));
  }

  var simvarCache = Object.create(null);
  var simvarCacheFrame = -1;
  var frameCounter = 0;

  function readSimVar(name, unit) {
    var key = name + "|" + (unit || "");
    if (simvarCacheFrame === frameCounter && key in simvarCache) {
      return simvarCache[key];
    }
    var result = request("op=simvar&name=" + encode(name) + "&unit=" + encode(unit || ""));
    var value = typeof result.value === "number" ? result.value : 0;
    simvarCache[key] = value;
    simvarCacheFrame = frameCounter;
    return value;
  }

  window.SimVar = {
    GetSimVarValue: function (name, unit) {
      return readSimVar(name, unit);
    },
    SetSimVarValue: function (name, unit, value) {
      request("op=simvar_set&name=" + encode(name) + "&value=" + encode(value));
      simvarCache = Object.create(null);
      return true;
    },
    GetGameVarValue: function (name) {
      var result = request("op=gamevar&name=" + encode(name));
      return typeof result.value === "number" ? result.value : 0;
    },
    GetRegisteredId: function (name) {
      var map = (window.__MSFS_REGISTERED_IDS__ = window.__MSFS_REGISTERED_IDS__ || {});
      if (!(name in map)) {
        map[name] = Object.keys(map).length + 1;
      }
      return map[name];
    },
    GetSimVarObject: function (name, unit) {
      return { name: name, unit: unit, value: readSimVar(name, unit) };
    }
  };

  var handlers = Object.create(null);

  window.Coherent = {
    call: function (name, ...args) {
      switch (name) {
        case "TRIGGER_KEY_EVENT":
        case "INTERCEPT_KEY_EVENT":
          if (name === "TRIGGER_KEY_EVENT" && args.length) {
            request("op=event&name=" + encode(args[0]));
          }
          return Promise.resolve(true);
        case "GET_METAR_BY_IDENT":
        case "GET_METAR_BY_LATLON":
        case "SEARCH_BY_IDENT":
        case "SEARCH_NEAREST":
        case "GET_NEAREST_AIRSPACES":
          return Promise.resolve({});
        case "SET_LOAD_LATLON":
        case "SET_MAP_PARAMS":
        case "SET_MAP_RESOLUTION":
        case "SET_MAP_CLEAR_COLOR":
        case "SET_MAP_ALTITUDE_RANGE":
        case "SET_MAP_HEIGHT_COLORS":
        case "SET_MAP_WEATHER_RADAR_COLORS":
        case "SET_NEAREST_AIRPORT_FILTER":
        case "SET_NEAREST_BOUNDARY_FILTER":
        case "SET_NEAREST_EXTENDED_AIRPORT_FILTERS":
        case "SET_NEAREST_INTERSECTION_FILTER":
        case "SET_NEAREST_VOR_FILTER":
        case "SHOW_MAP":
        case "SHOW_MAP_ISOLINES":
        case "SHOW_MAP_WEATHER":
        case "START_NEAREST_SEARCH_SESSION":
        case "PLAY_INSTRUMENT_SOUND":
          return Promise.resolve(true);
        default:
          return Promise.resolve(null);
      }
    },
    on: function (name, handler) {
      (handlers[name] = handlers[name] || []).push(handler);
    },
    off: function (name, handler) {
      var list = handlers[name];
      if (!list) return;
      handlers[name] = list.filter(function (h) { return h !== handler; });
    },
    trigger: function (name, ...args) {
      (handlers[name] || []).forEach(function (h) {
        try { h.apply(null, args); } catch (e) { console.error(e); }
      });
    },
    triggerAll: function (name, ...args) {
      window.Coherent.trigger(name, ...args);
    },
    DEBUG: false
  };

  window.Utils = {
    DEG: Math.PI / 180,
    RAD: 180 / Math.PI,
    Clamp: function (v, min, max) { return Math.min(max, Math.max(min, v)); },
    Lerp: function (a, b, t) { return a + (b - a) * t; },
    InverseLerp: function (a, b, v) { return a === b ? 0 : (v - a) / (b - a); },
    SmoothStep: function (a, b, v) {
      var t = Math.min(1, Math.max(0, (v - a) / (b - a)));
      return t * t * (3 - 2 * t);
    },
    Round: function (v, decimals) {
      var f = Math.pow(10, decimals || 0);
      return Math.round(v * f) / f;
    },
    ToFixed: function (v, digits) { return Number(v).toFixed(digits); },
    PadLeft: function (v, len, ch) { return String(v).padStart(len, ch || "0"); },
    IsFiniteNumber: function (v) { return typeof v === "number" && isFinite(v); },
    GetAngleDelta: function (a, b) {
      var d = (b - a + 360) % 360;
      return d > 180 ? d - 360 : d;
    },
    js: function (path, onload) {
      var script = document.createElement("script");
      script.src = path;
      if (onload) script.onload = onload;
      document.head.appendChild(script);
    }
  };

  var loadedScripts = Object.create(null);
  window.Include = {
    addScript: function (path, onload) {
      if (loadedScripts[path]) {
        if (onload) onload();
        return;
      }
      loadedScripts[path] = true;
      var script = document.createElement("script");
      script.src = BASE + "/instrument/" + SESSION + path;
      script.async = false;
      if (onload) script.onload = onload;
      document.head.appendChild(script);
    },
    addCss: function (path) {
      var link = document.createElement("link");
      link.rel = "stylesheet";
      link.href = BASE + "/instrument/" + SESSION + path;
      document.head.appendChild(link);
    },
    readFile: function () { return null; }
  };

  window.SetStoredData = function (key, value, store) {
    try { localStorage.setItem("msfs:" + (store || "default") + ":" + key, JSON.stringify(value)); } catch (e) {}
  };
  window.GetStoredData = function (key, store) {
    try {
      var raw = localStorage.getItem("msfs:" + (store || "default") + ":" + key);
      return raw === null ? null : JSON.parse(raw);
    } catch (e) {
      return null;
    }
  };
  window.DeleteStoredData = function (key, store) {
    try { localStorage.removeItem("msfs:" + (store || "default") + ":" + key); } catch (e) {}
  };

  var templates = Object.create(null);

  window.TemplateElement = {
    getTemplate: function (id) {
      if (templates[id]) return templates[id];
      var node = document.getElementById(id);
      return node ? node.innerHTML : "";
    }
  };

  function registerTemplate(id, html) {
    templates[id] = html;
  }
  window.__MSFS_TEMPLATES__ = templates;
  window.__MSFS_REGISTER_TEMPLATE__ = registerTemplate;

  var instruments = [];
  var instrumentRegistry = Object.create(null);

  class BaseInstrument extends HTMLElement {
    constructor() {
      super();
      this.__framerate = 0;
      this.__lastFrame = performance.now();
      this.__running = false;
      this.__listeners = [];
      this.__isInitialized = false;
    }
    get templateID() { return ""; }
    get isInteractive() { return false; }
    get instrumentIdentifier() { return this.__identifier || ""; }
    get instrumentIndex() { return 0; }
    get aircraft() { return window.__MSFS_AIRCRAFT__ || {}; }
    get framerate() { return this.__framerate; }
    get deltaTime() { return 0.016; }

    connectedCallback() {
      if (!this.__isInitialized) {
        this.__isInitialized = true;
        var template = this.templateID ? TemplateElement.getTemplate(this.templateID) : "";
        if (template && !this.childElementCount) {
          this.innerHTML = template;
        }
        try {
          this.Init();
        } catch (e) {
          console.error("instrument Init failed", e);
        }
      }
      this.startLoop();
    }

    disconnectedCallback() {
      this.__running = false;
      try { this.OnDisconnect(); } catch (e) {}
    }

    Init() {}
    Update() {}
    OnInteractionEvent(_args) {}
    onInteractionEvent(args) { this.OnInteractionEvent(args); }
    onFlightStart() {}
    onFlightEnd() {}
    onGameStateChanged() {}
    onAircraftLoaded() {}
    onPowerOn() {}
    onPowerOff() {}
    onSourceChanged() {}

    startLoop() {
      if (this.__running) return;
      this.__running = true;
      var self = this;
      function frame(now) {
        if (!self.__running) return;
        self.__framerate = now - self.__lastFrame;
        self.__lastFrame = now;
        frameCounter++;
        try {
          self.Update();
        } catch (e) {
          console.error("instrument Update failed", e);
        }
        requestAnimationFrame(frame);
      }
      requestAnimationFrame(frame);
    }

    registerListener(type, callback) {
      var listener = RegisterViewListener(type, callback);
      this.__listeners.push(listener);
      return listener;
    }

    addEventListenerToProp() {}
    getChildById(id) { return this.querySelector("#" + id); }
    getChildrenById(id) { return this.querySelectorAll("#" + id); }
    setAttribute() {}
  }

  window.BaseInstrument = BaseInstrument;
  window.FsBaseInstrument = BaseInstrument;

  window.registerInstrument = function (name, klass) {
    instrumentRegistry[name] = klass;
    var tag = "msfs-" + name.replace(/[^a-zA-Z0-9_-]/g, "-");
    if (!customElements.get(tag)) {
      customElements.define(tag, klass);
    }
    instruments.push({ name: name, tag: tag });
    var host = document.getElementById("InstrumentMount");
    if (host) {
      var element = document.createElement(tag);
      host.appendChild(element);
    }
  };

  window.unregisterInstrument = function (name) {
    delete instrumentRegistry[name];
  };

  window.__MSFS_INSTRUMENTS__ = instruments;

  var viewListeners = Object.create(null);

  window.RegisterViewListener = function (name, callback, _once) {
    var existing = viewListeners[name];
    if (!existing) {
      existing = viewListeners[name] = {
        name: name,
        __handlers: [],
        on: function (event, handler) {
          this.__handlers.push({ event: event, handler: handler });
        },
        off: function (event, handler) {
          this.__handlers = this.__handlers.filter(function (h) {
            return h.event !== event || h.handler !== handler;
          });
        },
        trigger: function (event, ...args) {
          this.__handlers.forEach(function (h) {
            if (h.event === event) {
              try { h.handler.apply(null, args); } catch (e) { console.error(e); }
            }
          });
        }
      };
    }
    if (callback) {
      setTimeout(function () {
        try { callback(existing); } catch (e) { console.error(e); }
      }, 0);
    }
    return existing;
  };

  var commbusListeners = Object.create(null);
  var commbusQueue = Object.create(null);
  var commbusCursor = 0;

  window.RegisterCommBusListener = function (callback) {
    var listener = commbusListeners["default"];
    if (!listener) {
      listener = commbusListeners["default"] = {
        __handlers: [],
        on: function (event, handler) {
          this.__handlers.push({ event: event, handler: handler });
        },
        off: function (event, handler) {
          this.__handlers = this.__handlers.filter(function (h) {
            return h.event !== event || h.handler !== handler;
          });
        },
        callWasm: function (event, payload) {
          request(
            "op=commbus_call&name=" + encode(event) + "&broadcast=" + (1 << 1),
            String(payload)
          );
        },
        callSimConnect: function (event, payload) {
          request(
            "op=commbus_call&name=" + encode(event) + "&broadcast=" + (1 << 2),
            String(payload)
          );
        },
        call: function (event, payload) {
          request(
            "op=commbus_call&name=" + encode(event) + "&broadcast=3",
            String(payload)
          );
        },
        dispatch: function (event, payload) {
          this.__handlers.forEach(function (h) {
            if (h.event === event) {
              try { h.handler(payload); } catch (e) { console.error(e); }
            }
          });
        }
      };
    }
    if (callback) {
      setTimeout(function () {
        try { callback(); } catch (e) { console.error(e); }
      }, 0);
    }
    return listener;
  };

  window.__MSFS_COMMBUS_DISPATCH__ = function (event, payload) {
    var listener = commbusListeners["default"];
    if (listener) listener.dispatch(event, payload);
  };

  setInterval(function () {
    var result = request("op=commbus_poll&since=" + commbusCursor);
    if (Array.isArray(result.items)) {
      result.items.forEach(function (item) {
        commbusCursor = Math.max(commbusCursor, item.seq || 0);
        window.__MSFS_COMMBUS_DISPATCH__(item.name, item.payload);
        Coherent.trigger("COMM_BUS_EVENT", item.name, item.payload);
      });
    }
  }, 50);

  window.SimplaneValues = {
    get: function (name) { return readSimVar(name, ""); }
  };

  window.Avionics = window.Avionics || {};

  window.FlightSimVar = {
    GetSimVarValue: window.SimVar.GetSimVarValue,
    SetSimVarValue: window.SimVar.SetSimVarValue
  };

  window.console.error = window.console.error.bind(window.console);

  window.addEventListener("error", function (e) {
    request("op=log&message=" + encode("uncaught: " + (e.message || e)));
  });
  window.addEventListener("unhandledrejection", function (e) {
    request("op=log&message=" + encode("unhandled rejection: " + (e.reason || e)));
  });

  document.addEventListener("DOMContentLoaded", function () {
    request("op=log&message=" + encode("instrument document ready"));
  });

  window.__MSFS_READY__ = true;
})();

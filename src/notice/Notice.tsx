import { emit } from "@tauri-apps/api/event";
import React, { useEffect, useReducer, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import "./Notice.css";
import { commands, events } from "@/bindings";
import type { LearningNotice } from "@/bindings";
import { useOsType } from "@/hooks/useOsType";
import { formatKeyCombination } from "@/lib/utils/keyboard";

/** Sent to Rust once the listener below is in place. Payload: notices seen. */
const READY_EVENT = "learning-notice-ready";
/** Sent to Rust when nothing is left to show. Payload: notices seen. */
const IDLE_EVENT = "learning-notice-idle";

/** How long one notice stays. The swap shortcut lives 6 s in the service. */
const SHOW_MS = 6000;
/** Used while more notices wait, so a burst does not take a minute. */
const SHOW_WITH_QUEUE_MS = 3500;
/** How long the answer to a click ("Undone") stays. */
const RESULT_MS = 1600;

/** The id of the shortcut binding the service registers during a swap notice. */
const SWAP_BINDING_ID = "learning_swap";

type Status = "open" | "undone" | "undoFailed" | "swapped" | "swapUnavailable";

interface Item {
  key: number;
  notice: LearningNotice;
  status: Status;
}

interface State {
  current: Item | null;
  queue: Item[];
}

type Action =
  | { type: "push"; item: Item }
  | { type: "settle"; key: number; status: Status }
  | { type: "dismiss"; key: number };

/** Shows the next waiting notice when none is showing. */
const promote = (state: State): State =>
  state.current === null && state.queue.length > 0
    ? { current: state.queue[0], queue: state.queue.slice(1) }
    : state;

const reducer = (state: State, action: Action): State => {
  switch (action.type) {
    case "push":
      return promote({ ...state, queue: [...state.queue, action.item] });
    case "settle":
      return state.current?.key === action.key
        ? { ...state, current: { ...state.current, status: action.status } }
        : state;
    case "dismiss":
      return state.current?.key === action.key
        ? promote({ ...state, current: null })
        : state;
  }
};

const Notice: React.FC = () => {
  const { t } = useTranslation();
  const osType = useOsType();
  const [state, dispatch] = useReducer(reducer, { current: null, queue: [] });
  const [swapHint, setSwapHint] = useState("");

  // How many notices arrived. Rust compares it with how many it sent, to know
  // that nothing is still on its way before it hides this window.
  const seenRef = useRef(0);
  const keyRef = useRef(0);
  const stateRef = useRef(state);
  stateRef.current = state;

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | null = null;
    events.learningNotice
      .listen((event) => {
        seenRef.current += 1;
        keyRef.current += 1;
        dispatch({
          type: "push",
          item: {
            key: keyRef.current,
            notice: event.payload,
            status: "open",
          },
        });
      })
      .then((stop) => {
        if (!active) {
          stop();
          return;
        }
        unlisten = stop;
        // Notices that came before this page listened are sent after this.
        void emit(READY_EVENT, seenRef.current);
      });
    return () => {
      active = false;
      unlisten?.();
    };
  }, []);

  // The current notice goes away after a while.
  const currentKey = state.current?.key;
  const currentStatus = state.current?.status;
  useEffect(() => {
    if (currentKey === undefined || currentStatus === undefined) return;
    const waiting = stateRef.current.queue.length > 0;
    const ms =
      currentStatus === "open"
        ? waiting
          ? SHOW_WITH_QUEUE_MS
          : SHOW_MS
        : RESULT_MS;
    const timer = setTimeout(
      () => dispatch({ type: "dismiss", key: currentKey }),
      ms,
    );
    return () => clearTimeout(timer);
  }, [currentKey, currentStatus]);

  // Nothing showing and nothing waiting: Rust may hide the window.
  const empty = state.current === null && state.queue.length === 0;
  useEffect(() => {
    if (empty && seenRef.current > 0) {
      void emit(IDLE_EVENT, seenRef.current);
    }
  }, [empty]);

  // The swap shortcut, for the hint under an "Alternative" notice.
  const isAlternative = state.current?.notice.kind === "Alternative";
  useEffect(() => {
    if (!isAlternative) return;
    let active = true;
    commands
      .getAppSettings()
      .then((result) => {
        if (!active || result.status !== "ok") return;
        const binding =
          result.data.bindings?.[SWAP_BINDING_ID]?.current_binding ?? "";
        setSwapHint(formatKeyCombination(binding, osType));
      })
      .catch((error) =>
        console.error("Failed to read the swap shortcut", error),
      );
    return () => {
      active = false;
    };
  }, [isAlternative, currentKey, osType]);

  const current = state.current;

  const undo = async (item: Item, ruleId: number) => {
    const result = await commands.learningUndo(ruleId).catch(() => null);
    dispatch({
      type: "settle",
      key: item.key,
      status: result?.status === "ok" ? "undone" : "undoFailed",
    });
  };

  const swap = async (item: Item, sessionId: number, index: number) => {
    const result = await commands
      .learningSwap(sessionId, index)
      .catch(() => null);
    if (result?.status === "ok") {
      dispatch({ type: "settle", key: item.key, status: "swapped" });
      return;
    }
    // A failed write also sends a "SwapFailed" notice that says more. Do not
    // say the same thing twice.
    const explained = stateRef.current.queue.some(
      (queued) => queued.notice.kind === "SwapFailed",
    );
    dispatch(
      explained
        ? { type: "dismiss", key: item.key }
        : { type: "settle", key: item.key, status: "swapUnavailable" },
    );
  };

  if (current === null) {
    return <div className="nt-stage" />;
  }

  const { notice, status } = current;
  let body: React.ReactNode;

  if (status === "undone") {
    body = <p className="nt-text">{t("notice.undone")}</p>;
  } else if (status === "undoFailed") {
    body = <p className="nt-text">{t("notice.undoFailed")}</p>;
  } else if (status === "swapped") {
    body = <p className="nt-text">{t("notice.swapped")}</p>;
  } else if (status === "swapUnavailable") {
    body = <p className="nt-text">{t("notice.swapUnavailable")}</p>;
  } else if (notice.kind === "Learned") {
    body = (
      <>
        <p className="nt-text">
          {t("notice.learned", { from: notice.from, to: notice.to })}
        </p>
        <button
          type="button"
          className="nt-btn"
          onClick={() => void undo(current, notice.rule_id)}
        >
          {t("notice.undo")}
        </button>
      </>
    );
  } else if (notice.kind === "Alternative") {
    body = (
      <>
        <div className="nt-text">
          <p>{t("notice.pasted", { chosen: notice.chosen })}</p>
          {swapHint !== "" && (
            <p className="nt-hint">
              {t("notice.swapHint", { shortcut: swapHint })}
            </p>
          )}
        </div>
        <button
          type="button"
          className="nt-btn"
          title={t("notice.swapTo", { alternative: notice.alternative })}
          onClick={() =>
            void swap(current, notice.session_id, notice.application_index)
          }
        >
          {t("notice.swapTo", { alternative: notice.alternative })}
        </button>
      </>
    );
  } else {
    body = (
      <p className="nt-text">
        {t("notice.swapFailed", { alternative: notice.alternative })}
      </p>
    );
  }

  return (
    <div className="nt-stage">
      {/* Keyed so each notice replays the pop-in. */}
      <div className="nt-card" key={`${current.key}-${status}`} role="status">
        {body}
      </div>
    </div>
  );
};

export default Notice;

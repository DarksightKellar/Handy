import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { ArrowRight, Check, Pencil, Trash2, X } from "lucide-react";
import { toast } from "sonner";
import { commands, events, type Rule } from "@/bindings";
import Badge from "../../ui/Badge";
import { Button } from "../../ui/Button";
import { Dialog } from "../../ui/Dialog";
import { Input } from "../../ui/Input";

const IconButton: React.FC<{
  onClick: () => void;
  title: string;
  disabled?: boolean;
  children: React.ReactNode;
}> = ({ onClick, title, disabled, children }) => (
  <button
    type="button"
    onClick={onClick}
    disabled={disabled}
    aria-label={title}
    title={title}
    className="p-1.5 rounded-md flex items-center justify-center transition-colors cursor-pointer text-text/50 hover:text-logo-primary disabled:cursor-not-allowed disabled:text-text/20"
  >
    {children}
  </button>
);

/** A small on/off switch, the same look as `ToggleSwitch` without its frame. */
const ActiveSwitch: React.FC<{
  checked: boolean;
  onChange: (checked: boolean) => void;
  label: string;
  disabled?: boolean;
}> = ({ checked, onChange, label, disabled }) => (
  <label
    className={`flex items-center ${disabled ? "cursor-not-allowed" : "cursor-pointer"}`}
    title={label}
  >
    <input
      type="checkbox"
      role="switch"
      aria-label={label}
      className="sr-only peer"
      checked={checked}
      disabled={disabled}
      onChange={(event) => onChange(event.target.checked)}
    />
    <div className="relative w-9 h-5 bg-mid-gray/20 peer-focus-visible:ring-2 peer-focus-visible:ring-logo-primary rounded-full peer peer-checked:after:translate-x-full rtl:peer-checked:after:-translate-x-full peer-checked:after:border-white after:content-[''] after:absolute after:top-[2px] after:start-[2px] after:bg-white after:border-gray-300 after:border after:rounded-full after:h-4 after:w-4 after:transition-all peer-checked:bg-background-ui peer-disabled:opacity-50" />
  </label>
);

interface RuleRowProps {
  rule: Rule;
  onChanged: (rule: Rule) => void;
  onDelete: (rule: Rule) => void;
}

const RuleRow: React.FC<RuleRowProps> = ({ rule, onChanged, onDelete }) => {
  const { t } = useTranslation();
  const [editing, setEditing] = useState(false);
  const [from, setFrom] = useState(rule.from);
  const [to, setTo] = useState(rule.to);
  const [busy, setBusy] = useState(false);

  const update = async (nextFrom: string, nextTo: string, paused: boolean) => {
    setBusy(true);
    try {
      const result = await commands.learningUpdateRule(
        rule.id,
        nextFrom,
        nextTo,
        paused,
      );
      if (result.status === "ok") {
        onChanged(result.data);
        return true;
      }
      toast.error(t("settings.learned.rules.updateError"), {
        description: result.error,
      });
    } catch (error) {
      console.error("Failed to update the rule:", error);
      toast.error(t("settings.learned.rules.updateError"));
    } finally {
      setBusy(false);
    }
    return false;
  };

  const startEditing = () => {
    setFrom(rule.from);
    setTo(rule.to);
    setEditing(true);
  };

  const save = async () => {
    if (await update(from, to, rule.paused)) {
      setEditing(false);
    }
  };

  const canSave = !busy && from.trim() !== "" && to.trim() !== "";

  if (editing) {
    return (
      <div className="px-4 py-3 flex flex-wrap items-center gap-2">
        <Input
          variant="compact"
          value={from}
          onChange={(event) => setFrom(event.target.value)}
          aria-label={t("settings.learned.rules.from")}
          placeholder={t("settings.learned.rules.from")}
          className="flex-1 min-w-32"
          autoFocus
        />
        <ArrowRight width={14} height={14} className="shrink-0 text-text/50" />
        <Input
          variant="compact"
          value={to}
          onChange={(event) => setTo(event.target.value)}
          aria-label={t("settings.learned.rules.to")}
          placeholder={t("settings.learned.rules.to")}
          className="flex-1 min-w-32"
          onKeyDown={(event) => {
            if (event.key === "Enter" && canSave) void save();
          }}
        />
        <div className="flex items-center gap-1">
          <Button
            size="sm"
            onClick={() => void save()}
            disabled={!canSave}
            className="flex items-center gap-1"
          >
            <Check width={14} height={14} />
            {t("settings.learned.rules.save")}
          </Button>
          <Button
            size="sm"
            variant="secondary"
            onClick={() => setEditing(false)}
            disabled={busy}
            className="flex items-center gap-1"
          >
            <X width={14} height={14} />
            {t("settings.learned.rules.cancel")}
          </Button>
        </div>
      </div>
    );
  }

  return (
    <div
      className={`px-4 py-3 flex items-center gap-3 ${rule.paused ? "opacity-60" : ""}`}
      title={rule.source_sentence}
    >
      <div className="flex-1 min-w-0">
        <p className="text-sm font-medium flex items-center gap-2 min-w-0">
          <span className="truncate">{rule.from}</span>
          <ArrowRight
            width={14}
            height={14}
            className="shrink-0 text-text/50"
            aria-hidden="true"
          />
          <span className="truncate">{rule.to}</span>
        </p>
        <p className="text-xs text-text/60 mt-0.5">
          {t("settings.learned.rules.stats", {
            hits: rule.hits,
            misses: rule.misses,
            applied: rule.applied,
          })}
        </p>
      </div>
      <Badge variant="secondary" className="shrink-0">
        {t(`settings.learned.rules.kind.${rule.kind}`)}
      </Badge>
      <ActiveSwitch
        checked={!rule.paused}
        onChange={(active) => void update(rule.from, rule.to, !active)}
        label={t("settings.learned.rules.active")}
        disabled={busy}
      />
      <div className="flex items-center">
        <IconButton
          onClick={startEditing}
          disabled={busy}
          title={t("settings.learned.rules.edit")}
        >
          <Pencil width={16} height={16} />
        </IconButton>
        <IconButton
          onClick={() => onDelete(rule)}
          disabled={busy}
          title={t("settings.learned.rules.delete")}
        >
          <Trash2 width={16} height={16} />
        </IconButton>
      </div>
    </div>
  );
};

export const RulesList: React.FC = () => {
  const { t } = useTranslation();
  const [rules, setRules] = useState<Rule[] | null>(null);
  const [failed, setFailed] = useState(false);
  const [deleting, setDeleting] = useState<Rule | null>(null);

  const refresh = useCallback(async () => {
    try {
      const result = await commands.learningListRules();
      if (result.status === "ok") {
        setRules(result.data);
        setFailed(false);
      } else {
        console.error("Failed to list the learned fixes:", result.error);
        setFailed(true);
      }
    } catch (error) {
      console.error("Failed to list the learned fixes:", error);
      setFailed(true);
    }
  }, []);

  // Load on open, when the window gets focus back (a fix may have been learned
  // or undone meanwhile), and when a new fix is learned.
  useEffect(() => {
    void refresh();
    window.addEventListener("focus", refresh);
    const unlisten = events.learningNotice.listen((event) => {
      if (event.payload.kind === "Learned") void refresh();
    });
    return () => {
      window.removeEventListener("focus", refresh);
      void unlisten.then((stop) => stop());
    };
  }, [refresh]);

  const replace = (rule: Rule) =>
    setRules((current) =>
      current ? current.map((r) => (r.id === rule.id ? rule : r)) : current,
    );

  const confirmDelete = async () => {
    const rule = deleting;
    if (!rule) return;
    setDeleting(null);
    try {
      const result = await commands.learningDeleteRule(rule.id);
      if (result.status === "ok") {
        setRules((current) =>
          current ? current.filter((r) => r.id !== rule.id) : current,
        );
        return;
      }
      toast.error(t("settings.learned.rules.deleteError"), {
        description: result.error,
      });
    } catch (error) {
      console.error("Failed to delete the rule:", error);
      toast.error(t("settings.learned.rules.deleteError"));
    }
    void refresh();
  };

  let content: React.ReactNode;
  if (failed && rules === null) {
    content = (
      <div className="px-4 py-3 text-center text-text/60">
        {t("settings.learned.rules.loadError")}
      </div>
    );
  } else if (rules === null) {
    content = (
      <div className="px-4 py-3 text-center text-text/60">
        {t("settings.learned.rules.loading")}
      </div>
    );
  } else if (rules.length === 0) {
    content = (
      <div className="px-4 py-3 text-center text-text/60">
        {t("settings.learned.rules.empty")}
      </div>
    );
  } else {
    content = (
      <div className="divide-y divide-mid-gray/20">
        {rules.map((rule) => (
          <RuleRow
            key={rule.id}
            rule={rule}
            onChanged={replace}
            onDelete={setDeleting}
          />
        ))}
      </div>
    );
  }

  return (
    <div className="space-y-2">
      <div className="px-4">
        <h2 className="text-xs font-medium text-mid-gray uppercase tracking-wide">
          {t("settings.learned.rules.title")}
        </h2>
        <p className="text-xs text-mid-gray mt-1">
          {t("settings.learned.rules.description")}
        </p>
      </div>
      <div className="bg-background border border-mid-gray/20 rounded-lg overflow-visible">
        {content}
      </div>
      <Dialog
        open={deleting !== null}
        title={t("settings.learned.rules.deleteTitle")}
        closeLabel={t("common.close")}
        onOpenChange={(open) => {
          if (!open) setDeleting(null);
        }}
        contentFades={false}
        footer={
          <>
            <Button variant="secondary" onClick={() => setDeleting(null)}>
              {t("settings.learned.rules.cancel")}
            </Button>
            <Button variant="danger" onClick={() => void confirmDelete()}>
              {t("common.delete")}
            </Button>
          </>
        }
      >
        {deleting && (
          <p className="text-sm text-text/80">
            {t("settings.learned.rules.deleteDescription", {
              from: deleting.from,
              to: deleting.to,
            })}
          </p>
        )}
      </Dialog>
    </div>
  );
};

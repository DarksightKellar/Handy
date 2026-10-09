import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Eye, EyeOff } from "lucide-react";
import { type } from "@tauri-apps/plugin-os";
import { useSettings } from "../../../hooks/useSettings";
import { Input } from "../../ui/Input";
import { SettingContainer } from "../../ui/SettingContainer";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { Slider } from "../../ui/Slider";
import { ToggleSwitch } from "../../ui/ToggleSwitch";
import { ShortcutInput } from "../ShortcutInput";
import { RulesList } from "./RulesList";

/** Watch time limits, in seconds. The backend clamps to the same range. */
const WATCH_MIN = 5;
const WATCH_MAX = 120;
const WATCH_DEFAULT = 30;

/** Jev confidence limits and default. The backend clamps to the same range. */
const THRESHOLD_MIN = 0.5;
const THRESHOLD_MAX = 0.99;
const THRESHOLD_DEFAULT = 0.8;

const WatchSeconds: React.FC<{ disabled: boolean }> = ({ disabled }) => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const saved = getSetting("learning_watch_seconds") ?? WATCH_DEFAULT;
  // Typing "30" passes through "3", which is out of range: keep the text in a
  // draft and save it when the field is left.
  const [draft, setDraft] = useState(String(saved));

  useEffect(() => {
    setDraft(String(saved));
  }, [saved]);

  const commit = () => {
    const value = Number.parseInt(draft, 10);
    if (Number.isNaN(value)) {
      setDraft(String(saved));
      return;
    }
    const clamped = Math.min(WATCH_MAX, Math.max(WATCH_MIN, value));
    setDraft(String(clamped));
    if (clamped !== saved) {
      void updateSetting("learning_watch_seconds", clamped);
    }
  };

  return (
    <SettingContainer
      title={t("settings.learned.watchSeconds.title")}
      description={t("settings.learned.watchSeconds.description")}
      descriptionMode="tooltip"
      grouped={true}
      layout="horizontal"
      disabled={disabled}
    >
      <div className="flex items-center space-x-2">
        <Input
          type="number"
          min={WATCH_MIN}
          max={WATCH_MAX}
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          onBlur={commit}
          onKeyDown={(event) => {
            if (event.key === "Enter") event.currentTarget.blur();
          }}
          disabled={disabled || isUpdating("learning_watch_seconds")}
          className="w-20"
        />
        <span className="text-sm text-text">
          {t("settings.learned.watchSeconds.unit")}
        </span>
      </div>
    </SettingContainer>
  );
};

const JevApiKey: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const saved = getSetting("jev_api_key") ?? "";
  const [draft, setDraft] = useState(saved);
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    setDraft(saved);
  }, [saved]);

  // Saved when the field is left, not on every key press.
  const commit = () => {
    const key = draft.trim();
    setDraft(key);
    if (key !== saved) {
      void updateSetting("jev_api_key", key);
    }
  };

  return (
    <SettingContainer
      title={t("settings.learned.jev.apiKey.title")}
      description={t("settings.learned.jev.apiKey.description")}
      descriptionMode="tooltip"
      grouped={true}
      layout="horizontal"
    >
      <div className="flex items-center space-x-1">
        <Input
          type={visible ? "text" : "password"}
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          onBlur={commit}
          onKeyDown={(event) => {
            if (event.key === "Enter") event.currentTarget.blur();
          }}
          placeholder={t("settings.learned.jev.apiKey.placeholder")}
          autoComplete="off"
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          disabled={isUpdating("jev_api_key")}
          className="w-56"
        />
        <button
          type="button"
          onClick={() => setVisible((shown) => !shown)}
          aria-label={
            visible
              ? t("settings.learned.jev.apiKey.hide")
              : t("settings.learned.jev.apiKey.show")
          }
          aria-pressed={visible}
          title={
            visible
              ? t("settings.learned.jev.apiKey.hide")
              : t("settings.learned.jev.apiKey.show")
          }
          className="p-1.5 rounded-md text-text/50 hover:text-logo-primary transition-colors cursor-pointer"
        >
          {visible ? (
            <EyeOff width={16} height={16} />
          ) : (
            <Eye width={16} height={16} />
          )}
        </button>
      </div>
    </SettingContainer>
  );
};

export const LearnedSettings: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const enabled = getSetting("learning_enabled") ?? true;
  // The swap shortcut is registered on demand. That is unstable on Linux (the
  // cancel shortcut is hidden there for the same reason); the notice's Swap
  // button still works.
  const isLinux = type() === "linux";
  const threshold = getSetting("jev_confidence_threshold") ?? THRESHOLD_DEFAULT;

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup title={t("settings.learned.title")}>
        <ToggleSwitch
          checked={enabled}
          onChange={(checked) =>
            void updateSetting("learning_enabled", checked)
          }
          isUpdating={isUpdating("learning_enabled")}
          label={t("settings.learned.enabled.title")}
          description={t("settings.learned.enabled.description")}
          descriptionMode="tooltip"
          grouped={true}
        />
        <WatchSeconds disabled={!enabled} />
        {!isLinux && (
          <ShortcutInput shortcutId="learning_swap" grouped={true} />
        )}
      </SettingsGroup>

      <SettingsGroup
        title={t("settings.learned.jev.title")}
        description={t("settings.learned.jev.privacy")}
      >
        <JevApiKey />
        <Slider
          value={threshold}
          onChange={(value) =>
            void updateSetting("jev_confidence_threshold", value)
          }
          min={THRESHOLD_MIN}
          max={THRESHOLD_MAX}
          step={0.01}
          label={t("settings.learned.jev.threshold.title")}
          description={t("settings.learned.jev.threshold.description")}
          descriptionMode="tooltip"
          grouped={true}
          formatValue={(value) => `${Math.round(value * 100)}%`}
        />
      </SettingsGroup>

      <RulesList />
    </div>
  );
};

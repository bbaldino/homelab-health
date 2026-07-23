import { useEffect, useState } from "preact/hooks";
import { api, ApiError } from "../api";
import type { CheckTypeSchema, Field, MonitorStatus, NewMonitor } from "../types";
import { SchemaField, humanize } from "./SchemaField";

/** In-form representation of a config value: string for text/number kinds, boolean for bool. */
type FieldValue = string | boolean;

interface MonitorFormProps {
  mode: "add" | "edit";
  /** Required when mode === "edit"; the monitor being edited. */
  monitor?: MonitorStatus;
  onSubmit: (payload: NewMonitor) => Promise<void>;
  onCancel: () => void;
}

function initialFieldValue(field: Field, existing: unknown): FieldValue {
  const raw = existing !== undefined ? existing : field.default;
  if (field.kind === "bool") return Boolean(raw);
  if (raw === null || raw === undefined) return "";
  return String(raw);
}

function buildInitialConfig(
  fields: Field[],
  existing?: Record<string, unknown>,
): Record<string, FieldValue> {
  const out: Record<string, FieldValue> = {};
  for (const field of fields) {
    out[field.name] = initialFieldValue(field, existing?.[field.name]);
  }
  return out;
}

/** Coerces a raw form value to the JSON type its schema field declares. */
function coerceFieldValue(field: Field, raw: FieldValue | undefined): unknown {
  if (field.kind === "bool") return Boolean(raw);
  const str = typeof raw === "string" ? raw.trim() : "";
  if (str === "") return null;
  if (field.kind === "int") {
    const n = parseInt(str, 10);
    return Number.isFinite(n) ? n : null;
  }
  if (field.kind === "float") {
    const n = parseFloat(str);
    return Number.isFinite(n) ? n : null;
  }
  return str;
}

export function MonitorForm({ mode, monitor, onSubmit, onCancel }: MonitorFormProps) {
  const [checkTypes, setCheckTypes] = useState<CheckTypeSchema[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  const [typeId, setTypeId] = useState(monitor?.type_id ?? "");
  const [name, setName] = useState(monitor?.name ?? "");
  const [intervalSecs, setIntervalSecs] = useState(
    monitor ? String(monitor.interval_secs) : "60",
  );
  const [enabled, setEnabled] = useState(monitor?.enabled ?? true);
  const [configValues, setConfigValues] = useState<Record<string, FieldValue>>({});

  const [validationError, setValidationError] = useState<string | null>(null);
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    let cancelled = false;
    api
      .getCheckTypes()
      .then((types) => {
        if (cancelled) return;
        setCheckTypes(types);
        if (mode === "edit" && monitor) {
          const schema = types.find((t) => t.type_id === monitor.type_id);
          setConfigValues(buildInitialConfig(schema?.schema.fields ?? [], monitor.config));
        } else if (mode === "add" && types.length > 0) {
          setTypeId(types[0].type_id);
          setConfigValues(buildInitialConfig(types[0].schema.fields));
        }
      })
      .catch((err) => {
        if (cancelled) return;
        setLoadError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const selectedSchema = checkTypes?.find((t) => t.type_id === typeId);
  const fields = selectedSchema?.schema.fields ?? [];

  function handleTypeChange(newTypeId: string) {
    setTypeId(newTypeId);
    const schema = checkTypes?.find((t) => t.type_id === newTypeId);
    setConfigValues(buildInitialConfig(schema?.schema.fields ?? []));
  }

  async function handleSubmit(e: Event) {
    e.preventDefault();
    setSubmitError(null);

    const trimmedName = name.trim();
    const parsedInterval = Number(intervalSecs);

    const missing: string[] = [];
    if (!trimmedName) missing.push("Name");
    if (!typeId) missing.push("Check type");
    if (!intervalSecs || !Number.isFinite(parsedInterval) || parsedInterval <= 0) {
      missing.push("Interval");
    }

    const config: Record<string, unknown> = {};
    for (const field of fields) {
      const coerced = coerceFieldValue(field, configValues[field.name]);
      config[field.name] = coerced;
      if (field.required && (coerced === null || coerced === "")) {
        missing.push(humanize(field.name));
      }
    }

    if (missing.length > 0) {
      setValidationError(
        `Missing or invalid: ${missing.join(", ")}`,
      );
      return;
    }
    setValidationError(null);

    const payload: NewMonitor = {
      name: trimmedName,
      type_id: typeId,
      config,
      interval_secs: parsedInterval,
      enabled,
    };

    setSubmitting(true);
    try {
      await onSubmit(payload);
    } catch (err) {
      setSubmitError(
        err instanceof ApiError
          ? err.message
          : err instanceof Error
            ? err.message
            : String(err),
      );
    } finally {
      setSubmitting(false);
    }
  }

  if (loadError) {
    return <div class="form-error">Failed to load check types: {loadError}</div>;
  }

  if (!checkTypes) {
    return <p class="form-loading">Loading check types…</p>;
  }

  return (
    <form class="monitor-form" onSubmit={handleSubmit}>
      <div class="form-field">
        <label class="field-label" for="monitor-name">
          Name<span class="required-marker">*</span>
        </label>
        <input
          id="monitor-name"
          type="text"
          value={name}
          autoFocus
          onInput={(e) => setName(e.currentTarget.value)}
        />
      </div>

      <div class="form-field">
        <label class="field-label" for="monitor-type">
          Check type<span class="required-marker">*</span>
        </label>
        <select
          id="monitor-type"
          value={typeId}
          disabled={mode === "edit"}
          onChange={(e) => handleTypeChange(e.currentTarget.value)}
        >
          {checkTypes.map((t) => (
            <option key={t.type_id} value={t.type_id}>
              {t.type_id}
            </option>
          ))}
        </select>
        {mode === "edit" && (
          <p class="field-help">Check type can&rsquo;t be changed after creation.</p>
        )}
      </div>

      {fields.length > 0 && (
        <fieldset class="schema-fields">
          <legend>Configuration</legend>
          {fields.map((field) => (
            <SchemaField
              key={field.name}
              field={field}
              value={configValues[field.name]}
              onChange={(v) =>
                setConfigValues((prev) => ({ ...prev, [field.name]: v as FieldValue }))
              }
            />
          ))}
        </fieldset>
      )}

      <div class="form-row">
        <div class="form-field">
          <label class="field-label" for="monitor-interval">
            Interval (seconds)<span class="required-marker">*</span>
          </label>
          <input
            id="monitor-interval"
            type="number"
            min="1"
            step="1"
            value={intervalSecs}
            onInput={(e) => setIntervalSecs(e.currentTarget.value)}
          />
        </div>

        <div class="form-field form-field-checkbox form-field-enabled">
          <label class="checkbox-label" for="monitor-enabled">
            <input
              id="monitor-enabled"
              type="checkbox"
              checked={enabled}
              onChange={(e) => setEnabled(e.currentTarget.checked)}
            />
            <span>Enabled</span>
          </label>
        </div>
      </div>

      {validationError && <div class="form-error">{validationError}</div>}
      {submitError && <div class="form-error">{submitError}</div>}

      <div class="form-actions">
        <button
          type="button"
          class="btn btn-secondary"
          onClick={onCancel}
          disabled={submitting}
        >
          Cancel
        </button>
        <button type="submit" class="btn btn-primary" disabled={submitting}>
          {submitting ? "Saving…" : mode === "add" ? "Add monitor" : "Save changes"}
        </button>
      </div>
    </form>
  );
}

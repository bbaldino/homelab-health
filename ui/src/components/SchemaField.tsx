import type { Field } from "../types";
import { ListField } from "./ListField";

interface SchemaFieldProps {
  field: Field;
  value: unknown;
  onChange: (value: unknown) => void;
  autoFocus?: boolean;
  /**
   * Row-aware suggestion source for a "list" field's sub-fields, e.g. the
   * prometheus rule builder's metric/label matcher autocomplete. Forwarded
   * to ListField when field.kind === "list"; ignored otherwise.
   */
  suggest?: (subFieldName: string, row: Record<string, unknown>) => string[];
  /**
   * Pre-resolved suggestion values for this field's own text input (set by
   * a parent ListField when rendering one row's sub-field). When present
   * and non-empty, a <datalist> is attached to the text input.
   */
  textSuggestions?: string[];
  /** id shared between the text input's `list` attribute and its <datalist>. */
  datalistId?: string;
}

function toInputValue(value: unknown): string {
  if (value === null || value === undefined) return "";
  return String(value);
}

export function humanize(name: string): string {
  if (!name) return name;
  const spaced = name.replace(/_/g, " ");
  return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}

/** Renders one input for a single check-type config field, driven by its schema. */
export function SchemaField({
  field,
  value,
  onChange,
  autoFocus,
  suggest,
  textSuggestions,
  datalistId,
}: SchemaFieldProps) {
  const inputId = `field-${field.name}`;
  const label = humanize(field.name);

  if (field.kind === "list") {
    return (
      <ListField
        field={field}
        value={Array.isArray(value) ? (value as Record<string, unknown>[]) : []}
        onChange={onChange}
        suggest={suggest}
      />
    );
  }

  if (field.options && field.kind !== "bool") {
    return (
      <div class="form-field">
        <label class="field-label" for={inputId}>
          {label}
          {field.required && <span class="required-marker">*</span>}
        </label>
        <select
          id={inputId}
          value={toInputValue(value)}
          autoFocus={autoFocus}
          onChange={(e) => onChange(e.currentTarget.value)}
        >
          <option value="" disabled>—</option>
          {field.options.map((opt) => (
            <option key={String(opt)} value={String(opt)}>{String(opt)}</option>
          ))}
        </select>
        {field.help && <p class="field-help">{field.help}</p>}
      </div>
    );
  }

  if (field.kind === "bool") {
    return (
      <div class="form-field form-field-checkbox">
        <label class="checkbox-label" for={inputId}>
          <input
            id={inputId}
            type="checkbox"
            checked={Boolean(value)}
            autoFocus={autoFocus}
            onChange={(e) => onChange(e.currentTarget.checked)}
          />
          <span>
            {label}
            {field.required && <span class="required-marker">*</span>}
          </span>
        </label>
        {field.help && <p class="field-help">{field.help}</p>}
      </div>
    );
  }

  const isNumber = field.kind === "int" || field.kind === "float";
  const hasSuggestions = !isNumber && !field.secret && (textSuggestions?.length ?? 0) > 0;

  return (
    <div class="form-field">
      <label class="field-label" for={inputId}>
        {label}
        {field.required && <span class="required-marker">*</span>}
      </label>
      <input
        id={inputId}
        type={field.secret ? "password" : isNumber ? "number" : "text"}
        step={field.kind === "float" ? "any" : undefined}
        value={toInputValue(value)}
        autoFocus={autoFocus}
        autoComplete={field.secret ? "off" : undefined}
        list={hasSuggestions ? datalistId : undefined}
        onInput={(e) => onChange(e.currentTarget.value)}
      />
      {hasSuggestions && (
        <datalist id={datalistId}>
          {textSuggestions!.map((s) => (
            <option key={s} value={s} />
          ))}
        </datalist>
      )}
      {field.help && <p class="field-help">{field.help}</p>}
    </div>
  );
}

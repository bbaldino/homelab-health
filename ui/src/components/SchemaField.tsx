import type { Field } from "../types";

interface SchemaFieldProps {
  field: Field;
  value: unknown;
  onChange: (value: unknown) => void;
  autoFocus?: boolean;
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
export function SchemaField({ field, value, onChange, autoFocus }: SchemaFieldProps) {
  const inputId = `field-${field.name}`;
  const label = humanize(field.name);

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
        onInput={(e) => onChange(e.currentTarget.value)}
      />
      {field.help && <p class="field-help">{field.help}</p>}
    </div>
  );
}

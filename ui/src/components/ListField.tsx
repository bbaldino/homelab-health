import type { Field } from "../types";
import { SchemaField, humanize } from "./SchemaField";

interface ListFieldProps {
  field: Field;
  value: Record<string, unknown>[];
  onChange: (value: unknown) => void;
  /**
   * Optional per-row suggestion source, e.g. metric/label matcher
   * autocomplete for the prometheus rule builder. Given a sub-field name
   * and the row it belongs to, returns candidate values for a <datalist>.
   * Absent for non-prometheus lists.
   */
  suggest?: (subFieldName: string, row: Record<string, unknown>) => string[];
}

function emptyRow(subFields: Field[]): Record<string, unknown> {
  const row: Record<string, unknown> = {};
  for (const f of subFields) {
    row[f.name] = f.kind === "bool" ? Boolean(f.default) : (f.default ?? "");
  }
  return row;
}

export function ListField({ field, value, onChange, suggest }: ListFieldProps) {
  const subFields = field.fields ?? [];
  const rows = value;

  function update(rows: Record<string, unknown>[]) {
    onChange(rows);
  }

  return (
    <div class="form-field list-field">
      <label class="field-label">{humanize(field.name)}</label>
      {field.help && <p class="field-help">{field.help}</p>}
      {rows.map((row, i) => (
        <div class="list-row" key={i}>
          {subFields.map((sf) => (
            <SchemaField
              key={sf.name}
              field={sf}
              value={row[sf.name]}
              onChange={(v) => {
                const next = rows.slice();
                next[i] = { ...row, [sf.name]: v };
                update(next);
              }}
              textSuggestions={suggest ? suggest(sf.name, row) : undefined}
              datalistId={`field-${field.name}-${i}-${sf.name}-list`}
            />
          ))}
          <button
            type="button"
            class="btn btn-secondary list-row-remove"
            onClick={() => update(rows.filter((_, j) => j !== i))}
            aria-label="Remove"
          >
            ✕
          </button>
        </div>
      ))}
      <button
        type="button"
        class="btn btn-secondary"
        onClick={() => update([...rows, emptyRow(subFields)])}
      >
        ＋ Add {humanize(field.name).replace(/s$/, "")}
      </button>
    </div>
  );
}

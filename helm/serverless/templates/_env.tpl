{{/*

Params:.
- key - String - Required
- value - Dict - Required.
*/}}
{{- define "common.env" -}}
{{ $key := include "common.tplvalues.render" ( dict "value" .key "context" .context ) -}}
{{ $value := include "common.tplvalues.render" ( dict "value" .value "context" .context ) -}}

{{ if .value  }}
{{ if not (include "common.inline.value" (dict "value" .value)) }}
name: {{ $key }}
valueFrom:
  {{ $value | nindent 2 }}
{{ else }}
name: {{ $key }}
value: {{ $value | quote }}
{{ end }}
{{ end }}
{{- end -}}

{{- define "common.inline.value" -}}
{{ if or ( kindIs "int" .value ) ( kindIs "string" .value ) ( kindIs "float64" .value ) }}
true
{{ end }}
{{- end -}}
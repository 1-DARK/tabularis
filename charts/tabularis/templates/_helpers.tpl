{{- define "tabularis.fullname" -}}
{{- if contains .Chart.Name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "tabularis.selectorLabels" -}}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "tabularis.labels" -}}
{{ include "tabularis.selectorLabels" . }}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{- define "tabularis.publicUrl" -}}
{{- required "publicUrl is required, e.g. --set publicUrl=https://tabularis.example.com" .Values.publicUrl | trimSuffix "/" -}}
{{- end -}}

{{/* host[:port] of the public URL: the only Host header the server answers. */}}
{{- define "tabularis.publicHost" -}}
{{- (urlParse (include "tabularis.publicUrl" .)).host -}}
{{- end -}}

{{- define "tabularis.image" -}}
{{- $tag := .Values.image.tag | default .Chart.AppVersion -}}
{{- if eq .Values.image.flavor "alpine" -}}
{{- $tag = printf "%s-alpine" $tag -}}
{{- else if ne .Values.image.flavor "debian" -}}
{{- fail "image.flavor must be debian or alpine" -}}
{{- end -}}
{{- printf "%s:%s" .Values.image.repository $tag -}}
{{- end -}}

{{- define "tabularis.secretName" -}}
{{- .Values.auth.existingSecret | default (include "tabularis.fullname" .) -}}
{{- end -}}

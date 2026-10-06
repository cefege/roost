{{- /* The release name alone when it already names the chart, so a release
       called roost-coordinator is not roost-coordinator-roost-coordinator. */}}
{{- define "roost.fullname" -}}
{{- if contains .Chart.Name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "roost.labels" -}}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version }}
{{- end -}}

{{- define "roost.selectorLabels" -}}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/component: coordinator
{{- end -}}

{{- define "roost.postgresName" -}}
{{- printf "%s-postgres" .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/* The Secret holding ROOST_COORDINATOR_DATABASE_URL, whichever source set it. */}}
{{- define "roost.databaseSecret" -}}
{{- if .Values.database.existingSecret -}}
{{- .Values.database.existingSecret -}}
{{- else -}}
{{- printf "%s-database" (include "roost.fullname" .) -}}
{{- end -}}
{{- end -}}

{{/* The URL this chart writes into its own Secret, or a refusal. */}}
{{- define "roost.databaseUrl" -}}
{{- if .Values.database.url -}}
{{- .Values.database.url -}}
{{- else if .Values.postgres.enabled -}}
{{- $password := required "postgres.password is required when postgres.enabled" .Values.postgres.password -}}
{{- printf "postgres://roost:%s@%s:5432/roost" (urlquery $password) (include "roost.postgresName" .) -}}
{{- else -}}
{{- fail "set database.url, database.existingSecret, or postgres.enabled" -}}
{{- end -}}
{{- end -}}

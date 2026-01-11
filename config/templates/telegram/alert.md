# ⚠️ Alert: {{ content.subject | default(value="System Alert") }}

*Priority:* P{{ message.priority | default(value=3) }}

{{ content.body | default(value="An alert was triggered.") }}

---
_Site: {{ site_id }}_
_Environment: {{ metadata.environment | default(value="unknown") }}_
_Time: {{ timestamp }}_

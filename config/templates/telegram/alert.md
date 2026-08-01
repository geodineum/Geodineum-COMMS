*⚠️ Alert: {{ content.subject | default(value="System Alert") | tg_escape }}*

*Priority:* P{{ message.priority | default(value=3) }}

{{ content.body | default(value="An alert was triggered.") | tg_escape }}

_Site: {{ site_id | tg_escape }}_
_Time: {{ timestamp | tg_escape }}_

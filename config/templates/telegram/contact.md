# New Contact

*{{ content.subject | default(value="Contact Form Submission") }}*

{{ content.body | default(value="No message") }}

---
*From:* {{ sender.name | default(value="Unknown") }}
{% if sender.email %}*Email:* {{ sender.email }}{% endif %}
{% if sender.phone %}*Phone:* {{ sender.phone }}{% endif %}

_Site: {{ site_id }} | {{ timestamp }}_

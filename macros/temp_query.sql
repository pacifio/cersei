{% macro temp_query(sql_query) %}
  {% set results = run_query(sql_query) %}
  {% if execute %}
    {% for row in results.rows %}
      {{ log(row.values(), info=True) }}
    {% endfor %}
  {% endif %}
{% endmacro %}
{% macro count_rows(table_name) %}
    {% set query %}
        SELECT COUNT(*) FROM {{ table_name }}
    {% endset %}
    {% set results = run_query(query) %}
    {% if execute %}
        {% do log(results.columns[0].values()[0], info=True) %}
    {% endif %}
{% endmacro %}
{{ config(materialized='table') }}

WITH medicare_specialties AS (
    SELECT
        medicare_specialty_code,
        medicare_provider_supplier_type_description,
        nucc_taxonomy_code
    FROM {{ source('nppes', 'medicare_specialty_crosswalk') }}
),

nucc_taxonomies AS (
    SELECT
        code AS taxonomy_code,
        specialization,
        classification,
        description AS nucc_description
    FROM {{ source('nppes', 'nucc_taxonomy') }}
),

joined_specialties AS (
    SELECT
        ms.medicare_specialty_code,
        ms.medicare_provider_supplier_type_description,
        nt.taxonomy_code,
        nt.specialization,
        nt.classification,
        nt.nucc_description,
        ROW_NUMBER() OVER (
            PARTITION BY ms.medicare_specialty_code
            ORDER BY
                CASE
                    WHEN nt.specialization IS NOT NULL AND nt.specialization != '' THEN 1
                    WHEN nt.classification IS NOT NULL AND nt.classification != '' THEN 2
                    WHEN nt.nucc_description IS NOT NULL AND nt.nucc_description != '' THEN 3
                    ELSE 4
                END,
                nt.taxonomy_code
        ) AS rn
    FROM medicare_specialties ms
    LEFT JOIN nucc_taxonomies nt
        ON ms.nucc_taxonomy_code = nt.taxonomy_code
)

SELECT
    taxonomy_code,
    medicare_specialty_code,
    COALESCE(
        medicare_provider_supplier_type_description,
        specialization,
        classification,
        nucc_description
    ) AS description
FROM joined_specialties
WHERE rn = 1

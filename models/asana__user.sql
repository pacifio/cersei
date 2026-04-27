
-- EXPECTED SHAPE: 20 rows — REASON: Based on unique stg_asana__user.user_id

with users as (

    select *
    from {{ ref('stg_asana__user') }}

), user_metrics as (

    select *
    from {{ ref('int_asana__user_task_metrics') }}

), final as (

    select
        users.user_id,
        users.user_name,
        users.email as user_email, -- Renamed from email to user_email based on yml contract
        -- derived from user_metrics
        coalesce(user_metrics.user_id is not null, false) as has_been_assigned_task,
        coalesce(user_metrics.number_of_open_tasks > 0, false) as is_currently_assigned_task,
        coalesce(user_metrics.number_of_open_tasks, 0) as number_of_open_tasks,
        coalesce(user_metrics.number_of_tasks_completed, 0) as number_of_completed_tasks,
        coalesce(user_metrics.avg_close_time_days, 0) as avg_completion_time_days

    from users
    left join user_metrics
        on users.user_id = user_metrics.user_id

)

select * from final

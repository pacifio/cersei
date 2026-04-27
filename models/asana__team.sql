
-- EXPECTED SHAPE: 25 rows — REASON: Based on unique team_data.id

with teams as (

    select
        id as team_id,
        name as team_name,
        organization_id as team_organization_id -- Directly from source to get this column
    from {{ source('asana', 'team_data') }}
    where not coalesce(_fivetran_deleted, false) -- Apply the same deletion filter as stg_asana__team

), projects as (

    select *
    from {{ ref('asana__project') }}

), agg_projects_by_team as (

    select
        team_id,
        sum(number_of_open_tasks) as number_of_open_tasks,
        sum(number_of_assigned_open_tasks) as number_of_assigned_open_tasks,
        sum(number_of_tasks_completed) as number_of_tasks_completed,
        avg(avg_close_time_days) as avg_close_time_days,
        avg(avg_close_time_assigned_days) as avg_close_time_assigned_days,
        {{ fivetran_utils.string_agg('project_name', "', '") }} as active_projects, -- Changed delimiter to match previous examples
        sum(case when not is_archived then 1 else 0 end) as number_of_active_projects,
        sum(case when is_archived then 1 else 0 end) as number_of_archived_projects
    from projects
    group by 1

), final as (

    select
        teams.team_id,
        teams.team_name,
        teams.team_organization_id,
        coalesce(agg_projects_by_team.number_of_open_tasks, 0) as number_of_open_tasks,
        coalesce(agg_projects_by_team.number_of_assigned_open_tasks, 0) as number_of_assigned_open_tasks,
        coalesce(agg_projects_by_team.number_of_tasks_completed, 0) as number_of_tasks_completed,
        coalesce(agg_projects_by_team.avg_close_time_days, 0) as avg_close_time_days,
        coalesce(agg_projects_by_team.avg_close_time_assigned_days, 0) as avg_close_time_assigned_days,
        agg_projects_by_team.active_projects,
        coalesce(agg_projects_by_team.number_of_active_projects, 0) as number_of_active_projects,
        coalesce(agg_projects_by_team.number_of_archived_projects, 0) as number_of_archived_projects
    from teams
    left join agg_projects_by_team
        on teams.team_id = agg_projects_by_team.team_id

)

select * from final

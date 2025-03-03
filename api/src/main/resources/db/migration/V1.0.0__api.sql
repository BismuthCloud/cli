CREATE TABLE public.api_keys (
    id bigint NOT NULL,
    userid bigint NOT NULL,
    token text NOT NULL,
    description text,
    createdat timestamp without time zone NOT NULL,
    updatedat timestamp without time zone NOT NULL
);




CREATE SEQUENCE public.api_keys_seq
    START WITH 1
    INCREMENT BY 50
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;




CREATE TABLE public.chat_messages (
    id bigint NOT NULL,
    content text DEFAULT ''::text NOT NULL,
    userid bigint,
    isai boolean DEFAULT false NOT NULL,
    containscode boolean DEFAULT false NOT NULL,
    createdat timestamp without time zone NOT NULL,
    updatedat timestamp without time zone NOT NULL,
    codeblockspans jsonb,
    feedbackupvote boolean,
    feedback text,
    messagellmcontext text,
    accepted boolean,
    sessionid bigint NOT NULL,
    requestid text
);




CREATE SEQUENCE public.chat_messages_seq
    START WITH 1
    INCREMENT BY 50
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;




CREATE TABLE public.chat_sessions (
    id bigint NOT NULL,
    featureid bigint NOT NULL,
    origin text NOT NULL,
    name text,
    createdat timestamp without time zone NOT NULL,
    updatedat timestamp without time zone NOT NULL,
    contextstorage jsonb
);




CREATE SEQUENCE public.chat_sessions_seq
    START WITH 1
    INCREMENT BY 50
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;




CREATE TABLE public.feature_config (
    id bigint NOT NULL,
    featureid bigint NOT NULL,
    key text NOT NULL,
    value text NOT NULL,
    createdat timestamp without time zone NOT NULL,
    updatedat timestamp without time zone NOT NULL
);




CREATE SEQUENCE public.feature_config_seq
    START WITH 1
    INCREMENT BY 50
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;




CREATE TABLE public.features (
    id bigint NOT NULL,
    name character varying(255),
    latestsavedchange character varying(255),
    projectid bigint NOT NULL,
    createdat timestamp without time zone NOT NULL,
    updatedat timestamp without time zone NOT NULL,
    functionuuid uuid,
    deployedcommit text
);




CREATE SEQUENCE public.features_seq
    START WITH 1
    INCREMENT BY 50
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;




CREATE TABLE public.generation_analysis (
    id bigint NOT NULL,
    createdat timestamp without time zone NOT NULL,
    updatedat timestamp without time zone NOT NULL,
    chatmessageid bigint NOT NULL,
    generation text NOT NULL,
    mypy text
);




CREATE SEQUENCE public.generation_analysis_seq
    START WITH 1
    INCREMENT BY 50
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;




CREATE TABLE public.generation_traces (
    id bigint NOT NULL,
    chatmessageid bigint NOT NULL,
    createdat timestamp without time zone NOT NULL,
    updatedat timestamp without time zone NOT NULL,
    state jsonb NOT NULL
);




CREATE SEQUENCE public.generation_traces_seq
    START WITH 1
    INCREMENT BY 50
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;




CREATE TABLE public.hourly_usage (
    id bigint NOT NULL,
    featureid bigint NOT NULL,
    orgid bigint NOT NULL,
    "time" timestamp with time zone NOT NULL,
    item text NOT NULL,
    usage bigint NOT NULL,
    createdat timestamp without time zone NOT NULL,
    updatedat timestamp without time zone NOT NULL
);




CREATE SEQUENCE public.hourly_usage_seq
    START WITH 1
    INCREMENT BY 50
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;




CREATE TABLE public.organization_users (
    userid bigint NOT NULL,
    orgid bigint NOT NULL
);




CREATE TABLE public.organizations (
    id bigint NOT NULL,
    name character varying(255),
    createdat timestamp without time zone NOT NULL,
    updatedat timestamp without time zone NOT NULL,
    subscriptionid bigint NOT NULL,
    llmconfig json
);




CREATE SEQUENCE public.organizations_seq
    START WITH 1
    INCREMENT BY 50
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;




CREATE TABLE public.projects (
    id bigint NOT NULL,
    name character varying(255),
    hash character varying(255),
    organizationid bigint NOT NULL,
    createdat timestamp without time zone NOT NULL,
    updatedat timestamp without time zone NOT NULL,
    internalclonetoken character varying(32),
    haspushed boolean DEFAULT false
);




CREATE SEQUENCE public.projects_seq
    START WITH 1
    INCREMENT BY 50
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;




CREATE TABLE public.subscriptions (
    createdat timestamp without time zone NOT NULL,
    updatedat timestamp without time zone NOT NULL,
    id bigint NOT NULL,
    customerid text,
    type text DEFAULT 'INDIVIDUAL'::text NOT NULL,
    expiresat timestamp without time zone,
    subscriptionid text,
    credits integer DEFAULT 0,
    bugscantype text DEFAULT 'FREE'::text,
    bugscanexpiresat timestamp without time zone,
    bugscansubscriptionid text
);




CREATE SEQUENCE public.subscriptions_seq
    START WITH 1
    INCREMENT BY 50
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;




CREATE TABLE public.users (
    id bigint NOT NULL,
    email character varying(255),
    name character varying(255),
    username character varying(255),
    createdat timestamp without time zone NOT NULL,
    updatedat timestamp without time zone NOT NULL,
    pending boolean DEFAULT false
);




CREATE SEQUENCE public.users_seq
    START WITH 1
    INCREMENT BY 50
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;




ALTER TABLE ONLY public.api_keys
    ADD CONSTRAINT api_keys_pkey PRIMARY KEY (id);



ALTER TABLE ONLY public.chat_messages
    ADD CONSTRAINT chat_messages_pkey PRIMARY KEY (id);



ALTER TABLE ONLY public.chat_sessions
    ADD CONSTRAINT chat_sessions_pkey PRIMARY KEY (id);



ALTER TABLE ONLY public.feature_config
    ADD CONSTRAINT feature_config_pkey PRIMARY KEY (id);



ALTER TABLE ONLY public.features
    ADD CONSTRAINT features_pkey PRIMARY KEY (id);



ALTER TABLE ONLY public.generation_analysis
    ADD CONSTRAINT generation_analysis_pkey PRIMARY KEY (id);



ALTER TABLE ONLY public.generation_traces
    ADD CONSTRAINT generation_traces_pkey PRIMARY KEY (id);



ALTER TABLE ONLY public.hourly_usage
    ADD CONSTRAINT hourly_usage_pkey PRIMARY KEY (id),
    ADD CONSTRAINT hourly_usage_time_item_featureid_key UNIQUE ("time", item, featureid);



ALTER TABLE ONLY public.organizations
    ADD CONSTRAINT organizations_pkey PRIMARY KEY (id);



ALTER TABLE ONLY public.projects
    ADD CONSTRAINT projects_pkey PRIMARY KEY (id);



ALTER TABLE ONLY public.subscriptions
    ADD CONSTRAINT subscriptions_pkey PRIMARY KEY (id);



ALTER TABLE ONLY public.feature_config
    ADD CONSTRAINT unique_featureid_key UNIQUE (featureid, key);



ALTER TABLE ONLY public.api_keys
    ADD CONSTRAINT unique_token UNIQUE (token);



ALTER TABLE ONLY public.users
    ADD CONSTRAINT users_pkey PRIMARY KEY (id);



ALTER TABLE ONLY public.generation_analysis
    ADD CONSTRAINT fk_chat_message FOREIGN KEY (chatmessageid) REFERENCES public.chat_messages(id) ON DELETE CASCADE;



ALTER TABLE ONLY public.generation_traces
    ADD CONSTRAINT fk_chat_message FOREIGN KEY (chatmessageid) REFERENCES public.chat_messages(id) ON DELETE CASCADE;



ALTER TABLE ONLY public.feature_config
    ADD CONSTRAINT fk_feature FOREIGN KEY (featureid) REFERENCES public.features(id) ON DELETE CASCADE;



ALTER TABLE ONLY public.chat_sessions
    ADD CONSTRAINT fk_feature FOREIGN KEY (featureid) REFERENCES public.features(id) ON DELETE CASCADE;



ALTER TABLE ONLY public.organization_users
    ADD CONSTRAINT fk_org FOREIGN KEY (orgid) REFERENCES public.organizations(id);



ALTER TABLE ONLY public.projects
    ADD CONSTRAINT fk_organization FOREIGN KEY (organizationid) REFERENCES public.organizations(id) ON DELETE CASCADE;



ALTER TABLE ONLY public.features
    ADD CONSTRAINT fk_project FOREIGN KEY (projectid) REFERENCES public.projects(id) ON DELETE CASCADE;



ALTER TABLE ONLY public.chat_messages
    ADD CONSTRAINT fk_session FOREIGN KEY (sessionid) REFERENCES public.chat_sessions(id) ON DELETE CASCADE;



ALTER TABLE ONLY public.organizations
    ADD CONSTRAINT fk_subscription FOREIGN KEY (subscriptionid) REFERENCES public.subscriptions(id);



ALTER TABLE ONLY public.chat_messages
    ADD CONSTRAINT fk_user FOREIGN KEY (userid) REFERENCES public.users(id);



ALTER TABLE ONLY public.organization_users
    ADD CONSTRAINT fk_user FOREIGN KEY (userid) REFERENCES public.users(id);



ALTER TABLE ONLY public.api_keys
    ADD CONSTRAINT fk_user FOREIGN KEY (userid) REFERENCES public.users(id);



CREATE SEQUENCE TASKS_SEQ start 1 increment 1;

CREATE TABLE TASKS
(
    id        int8 NOT NULL,
    createdAt TIMESTAMP,
    PRIMARY KEY (id)
);

CREATE TABLE QRTZ_JOB_DETAILS
(
    SCHED_NAME        VARCHAR(120) NOT NULL,
    JOB_NAME          VARCHAR(200) NOT NULL,
    JOB_GROUP         VARCHAR(200) NOT NULL,
    DESCRIPTION       VARCHAR(250) NULL,
    JOB_CLASS_NAME    VARCHAR(250) NOT NULL,
    IS_DURABLE        BOOL         NOT NULL,
    IS_NONCONCURRENT  BOOL         NOT NULL,
    IS_UPDATE_DATA    BOOL         NOT NULL,
    REQUESTS_RECOVERY BOOL         NOT NULL,
    JOB_DATA          BYTEA        NULL,
    PRIMARY KEY (SCHED_NAME, JOB_NAME, JOB_GROUP)
);

CREATE TABLE QRTZ_TRIGGERS
(
    SCHED_NAME     VARCHAR(120) NOT NULL,
    TRIGGER_NAME   VARCHAR(200) NOT NULL,
    TRIGGER_GROUP  VARCHAR(200) NOT NULL,
    JOB_NAME       VARCHAR(200) NOT NULL,
    JOB_GROUP      VARCHAR(200) NOT NULL,
    DESCRIPTION    VARCHAR(250) NULL,
    NEXT_FIRE_TIME BIGINT       NULL,
    PREV_FIRE_TIME BIGINT       NULL,
    PRIORITY       INTEGER      NULL,
    TRIGGER_STATE  VARCHAR(16)  NOT NULL,
    TRIGGER_TYPE   VARCHAR(8)   NOT NULL,
    START_TIME     BIGINT       NOT NULL,
    END_TIME       BIGINT       NULL,
    CALENDAR_NAME  VARCHAR(200) NULL,
    MISFIRE_INSTR  SMALLINT     NULL,
    JOB_DATA       BYTEA        NULL,
    PRIMARY KEY (SCHED_NAME, TRIGGER_NAME, TRIGGER_GROUP),
    FOREIGN KEY (SCHED_NAME, JOB_NAME, JOB_GROUP)
        REFERENCES QRTZ_JOB_DETAILS (SCHED_NAME, JOB_NAME, JOB_GROUP)
);

CREATE TABLE QRTZ_SIMPLE_TRIGGERS
(
    SCHED_NAME      VARCHAR(120) NOT NULL,
    TRIGGER_NAME    VARCHAR(200) NOT NULL,
    TRIGGER_GROUP   VARCHAR(200) NOT NULL,
    REPEAT_COUNT    BIGINT       NOT NULL,
    REPEAT_INTERVAL BIGINT       NOT NULL,
    TIMES_TRIGGERED BIGINT       NOT NULL,
    PRIMARY KEY (SCHED_NAME, TRIGGER_NAME, TRIGGER_GROUP),
    FOREIGN KEY (SCHED_NAME, TRIGGER_NAME, TRIGGER_GROUP)
        REFERENCES QRTZ_TRIGGERS (SCHED_NAME, TRIGGER_NAME, TRIGGER_GROUP)
);

CREATE TABLE QRTZ_CRON_TRIGGERS
(
    SCHED_NAME      VARCHAR(120) NOT NULL,
    TRIGGER_NAME    VARCHAR(200) NOT NULL,
    TRIGGER_GROUP   VARCHAR(200) NOT NULL,
    CRON_EXPRESSION VARCHAR(120) NOT NULL,
    TIME_ZONE_ID    VARCHAR(80),
    PRIMARY KEY (SCHED_NAME, TRIGGER_NAME, TRIGGER_GROUP),
    FOREIGN KEY (SCHED_NAME, TRIGGER_NAME, TRIGGER_GROUP)
        REFERENCES QRTZ_TRIGGERS (SCHED_NAME, TRIGGER_NAME, TRIGGER_GROUP)
);

CREATE TABLE QRTZ_SIMPROP_TRIGGERS
(
    SCHED_NAME    VARCHAR(120)   NOT NULL,
    TRIGGER_NAME  VARCHAR(200)   NOT NULL,
    TRIGGER_GROUP VARCHAR(200)   NOT NULL,
    STR_PROP_1    VARCHAR(512)   NULL,
    STR_PROP_2    VARCHAR(512)   NULL,
    STR_PROP_3    VARCHAR(512)   NULL,
    INT_PROP_1    INT            NULL,
    INT_PROP_2    INT            NULL,
    LONG_PROP_1   BIGINT         NULL,
    LONG_PROP_2   BIGINT         NULL,
    DEC_PROP_1    NUMERIC(13, 4) NULL,
    DEC_PROP_2    NUMERIC(13, 4) NULL,
    BOOL_PROP_1   BOOL           NULL,
    BOOL_PROP_2   BOOL           NULL,
    PRIMARY KEY (SCHED_NAME, TRIGGER_NAME, TRIGGER_GROUP),
    FOREIGN KEY (SCHED_NAME, TRIGGER_NAME, TRIGGER_GROUP)
        REFERENCES QRTZ_TRIGGERS (SCHED_NAME, TRIGGER_NAME, TRIGGER_GROUP)
);

CREATE TABLE QRTZ_BLOB_TRIGGERS
(
    SCHED_NAME    VARCHAR(120) NOT NULL,
    TRIGGER_NAME  VARCHAR(200) NOT NULL,
    TRIGGER_GROUP VARCHAR(200) NOT NULL,
    BLOB_DATA     BYTEA        NULL,
    PRIMARY KEY (SCHED_NAME, TRIGGER_NAME, TRIGGER_GROUP),
    FOREIGN KEY (SCHED_NAME, TRIGGER_NAME, TRIGGER_GROUP)
        REFERENCES QRTZ_TRIGGERS (SCHED_NAME, TRIGGER_NAME, TRIGGER_GROUP)
);

CREATE TABLE QRTZ_CALENDARS
(
    SCHED_NAME    VARCHAR(120) NOT NULL,
    CALENDAR_NAME VARCHAR(200) NOT NULL,
    CALENDAR      BYTEA        NOT NULL,
    PRIMARY KEY (SCHED_NAME, CALENDAR_NAME)
);


CREATE TABLE QRTZ_PAUSED_TRIGGER_GRPS
(
    SCHED_NAME    VARCHAR(120) NOT NULL,
    TRIGGER_GROUP VARCHAR(200) NOT NULL,
    PRIMARY KEY (SCHED_NAME, TRIGGER_GROUP)
);

CREATE TABLE QRTZ_FIRED_TRIGGERS
(
    SCHED_NAME        VARCHAR(120) NOT NULL,
    ENTRY_ID          VARCHAR(95)  NOT NULL,
    TRIGGER_NAME      VARCHAR(200) NOT NULL,
    TRIGGER_GROUP     VARCHAR(200) NOT NULL,
    INSTANCE_NAME     VARCHAR(200) NOT NULL,
    FIRED_TIME        BIGINT       NOT NULL,
    SCHED_TIME        BIGINT       NOT NULL,
    PRIORITY          INTEGER      NOT NULL,
    STATE             VARCHAR(16)  NOT NULL,
    JOB_NAME          VARCHAR(200) NULL,
    JOB_GROUP         VARCHAR(200) NULL,
    IS_NONCONCURRENT  BOOL         NULL,
    REQUESTS_RECOVERY BOOL         NULL,
    PRIMARY KEY (SCHED_NAME, ENTRY_ID)
);

CREATE TABLE QRTZ_SCHEDULER_STATE
(
    SCHED_NAME        VARCHAR(120) NOT NULL,
    INSTANCE_NAME     VARCHAR(200) NOT NULL,
    LAST_CHECKIN_TIME BIGINT       NOT NULL,
    CHECKIN_INTERVAL  BIGINT       NOT NULL,
    PRIMARY KEY (SCHED_NAME, INSTANCE_NAME)
);

CREATE TABLE QRTZ_LOCKS
(
    SCHED_NAME VARCHAR(120) NOT NULL,
    LOCK_NAME  VARCHAR(40)  NOT NULL,
    PRIMARY KEY (SCHED_NAME, LOCK_NAME)
);

CREATE INDEX IDX_QRTZ_J_REQ_RECOVERY
    ON QRTZ_JOB_DETAILS (SCHED_NAME, REQUESTS_RECOVERY);
CREATE INDEX IDX_QRTZ_J_GRP
    ON QRTZ_JOB_DETAILS (SCHED_NAME, JOB_GROUP);

CREATE INDEX IDX_QRTZ_T_J
    ON QRTZ_TRIGGERS (SCHED_NAME, JOB_NAME, JOB_GROUP);
CREATE INDEX IDX_QRTZ_T_JG
    ON QRTZ_TRIGGERS (SCHED_NAME, JOB_GROUP);
CREATE INDEX IDX_QRTZ_T_C
    ON QRTZ_TRIGGERS (SCHED_NAME, CALENDAR_NAME);
CREATE INDEX IDX_QRTZ_T_G
    ON QRTZ_TRIGGERS (SCHED_NAME, TRIGGER_GROUP);
CREATE INDEX IDX_QRTZ_T_STATE
    ON QRTZ_TRIGGERS (SCHED_NAME, TRIGGER_STATE);
CREATE INDEX IDX_QRTZ_T_N_STATE
    ON QRTZ_TRIGGERS (SCHED_NAME, TRIGGER_NAME, TRIGGER_GROUP, TRIGGER_STATE);
CREATE INDEX IDX_QRTZ_T_N_G_STATE
    ON QRTZ_TRIGGERS (SCHED_NAME, TRIGGER_GROUP, TRIGGER_STATE);
CREATE INDEX IDX_QRTZ_T_NEXT_FIRE_TIME
    ON QRTZ_TRIGGERS (SCHED_NAME, NEXT_FIRE_TIME);
CREATE INDEX IDX_QRTZ_T_NFT_ST
    ON QRTZ_TRIGGERS (SCHED_NAME, TRIGGER_STATE, NEXT_FIRE_TIME);
CREATE INDEX IDX_QRTZ_T_NFT_MISFIRE
    ON QRTZ_TRIGGERS (SCHED_NAME, MISFIRE_INSTR, NEXT_FIRE_TIME);
CREATE INDEX IDX_QRTZ_T_NFT_ST_MISFIRE
    ON QRTZ_TRIGGERS (SCHED_NAME, MISFIRE_INSTR, NEXT_FIRE_TIME, TRIGGER_STATE);
CREATE INDEX IDX_QRTZ_T_NFT_ST_MISFIRE_GRP
    ON QRTZ_TRIGGERS (SCHED_NAME, MISFIRE_INSTR, NEXT_FIRE_TIME, TRIGGER_GROUP,
                      TRIGGER_STATE);

CREATE INDEX IDX_QRTZ_FT_TRIG_INST_NAME
    ON QRTZ_FIRED_TRIGGERS (SCHED_NAME, INSTANCE_NAME);
CREATE INDEX IDX_QRTZ_FT_INST_JOB_REQ_RCVRY
    ON QRTZ_FIRED_TRIGGERS (SCHED_NAME, INSTANCE_NAME, REQUESTS_RECOVERY);
CREATE INDEX IDX_QRTZ_FT_J_G
    ON QRTZ_FIRED_TRIGGERS (SCHED_NAME, JOB_NAME, JOB_GROUP);
CREATE INDEX IDX_QRTZ_FT_JG
    ON QRTZ_FIRED_TRIGGERS (SCHED_NAME, JOB_GROUP);
CREATE INDEX IDX_QRTZ_FT_T_G
    ON QRTZ_FIRED_TRIGGERS (SCHED_NAME, TRIGGER_NAME, TRIGGER_GROUP);
CREATE INDEX IDX_QRTZ_FT_TG
    ON QRTZ_FIRED_TRIGGERS (SCHED_NAME, TRIGGER_GROUP);
\c quarkus

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

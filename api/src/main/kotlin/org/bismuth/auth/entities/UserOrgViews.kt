package org.bismuth.auth.entities

import com.fasterxml.jackson.databind.ObjectMapper
import io.quarkus.jackson.ObjectMapperCustomizer
import jakarta.inject.Singleton


class UserOrgViews {
    class UserView
    class OrganizationView
}

@Singleton
class ViewObjectMapperCustomizer : ObjectMapperCustomizer {
    override fun customize(mapper: ObjectMapper) {
        mapper.setConfig(mapper.serializationConfig.withView(UserOrgViews.UserView::class.java))
    }
}